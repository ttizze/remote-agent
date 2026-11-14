use crate::{HostCredentials, HostRpcService, SessionId};
use agent_protocol::{
    models::{HostDevice, HostStatus, Invitation, RemoteHost},
    protocol::{Body, Call, Response},
};
use agent_transport::transport::{
    Endpoint, HostPeer, IncomingRequest, IncomingSession, NodeId, Session, Ticket, authorize,
};
use anyhow::{Context, Result};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub struct HostRuntime {
    pub(crate) service: HostRpcService,
    endpoint: Endpoint,
    credentials: Arc<HostCredentials>,
    local_node: NodeId,
    name: String,
    invitation_lifetime: Duration,
    active: Mutex<BTreeMap<SessionId, Session>>,
    update_shutdown: CancellationToken,
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
        Self {
            service,
            endpoint,
            credentials,
            local_node,
            name,
            invitation_lifetime,
            active: Mutex::new(BTreeMap::new()),
            update_shutdown: CancellationToken::new(),
        }
    }
    pub fn ticket(&self) -> Ticket {
        self.endpoint.ticket()
    }
    pub async fn run(
        self: Arc<Self>,
        shutdown: CancellationToken,
        worktree_cleanup_interval: Duration,
    ) -> Result<()> {
        self.service.start().map_err(anyhow::Error::msg)?;
        let service = self.service.clone();
        let maintenance_stop = shutdown.clone();
        let maintenance = tokio::spawn(async move {
            let mut interval = tokio::time::interval(worktree_cleanup_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    _ = maintenance_stop.cancelled() => break,
                    _ = interval.tick() => {}
                    _ = service.worktree_cleanup_requested() => {}
                }
                if let Err(error) = service.cleanup_unchanged_worktrees().await {
                    tracing::warn!(target: "agent", operation = "host.worktree.cleanup", message = %error);
                }
            }
        });
        let mut sessions = JoinSet::new();
        let authorized_slots = Arc::new(tokio::sync::Semaphore::new(64));
        let pairing_slots = Arc::new(tokio::sync::Semaphore::new(16));
        let result = {
            // Preserve an in-flight handshake when another session finishes.
            let accept = self.endpoint.accept();
            tokio::pin!(accept);
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break Ok(()),
                    _ = self.update_shutdown.cancelled() => break Ok(()),
                    incoming = &mut accept => {
                        accept.set(self.endpoint.accept());
                        match incoming {
                            Some(Ok(incoming)) => {
                                let known = self.credentials.record.lock().await.trust.allowed.contains(&incoming.node_id());
                                let slots = if known { &authorized_slots } else { &pairing_slots };
                                let Ok(permit) = slots.clone().try_acquire_owned() else { continue; };
                                let runtime = self.clone();
                                let stop = shutdown.child_token();
                                let authorized_slots = authorized_slots.clone();
                                sessions.spawn(async move {
                                    runtime.serve(incoming, stop, permit, authorized_slots).await
                                });
                            }
                            Some(Err(error)) => tracing::error!(target: "agent", operation = "host.accept", message = %error),
                            None => break Ok(()),
                        }
                    },
                    Some(result) = sessions.join_next(), if !sessions.is_empty() => {
                        match result {
                            Ok(Err(error)) => tracing::error!(target: "agent", operation = "host.session", message = %format_args!("{error:#}")),
                            Err(error) => tracing::error!(target: "agent", operation = "host.session", message = %error),
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
        let accepting = connection.accept_request();
        tokio::pin!(accepting);
        let result = loop {
            tokio::select! {
                _ = stop.cancelled() => break Ok(()),
                incoming = &mut accepting => {
                    accepting.set(connection.accept_request());
                    match incoming {
                        Ok(IncomingRequest::Call(request)) => {
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
                                    tracing::info!(target: "agent", operation = "host.connection.link", message = %format_args!("session={} trace={} connection={} attempt={}", id, performance.timeline.id, performance.connection_id, performance.attempt_id));
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
                                        tracing::info!(target: "agent", operation = "host.rpc.performance", message);
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
                        Ok(IncomingRequest::Blob(stream)) => {
                            if transfers.len() >= 16 { continue; }
                            let service = self.service.clone();
                            transfers.spawn(async move { service.files().transfer(id, stream).await });
                        }
                        Ok(IncomingRequest::Close) => break Ok(()),
                        Err(error) => break Err(error.into()),
                    }
                },
                result = &mut outgoing => break result,
                Some(_) = requests.join_next(), if !requests.is_empty() => {},
                Some(_) = transfers.join_next(), if !transfers.is_empty() => {},
            }
        };
        requests.abort_all();
        transfers.abort_all();
        while requests.join_next().await.is_some() {}
        while transfers.join_next().await.is_some() {}
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
    ) -> Result<crate::host_rpc::routing::HostReply> {
        if matches!(message, Call::HostName(_)) {
            return Ok(Response::Success {
                result: Body::from(self.name.clone()),
            }
            .into());
        }
        let management = matches!(
            message,
            Call::Pair(_)
                | Call::IdentifyDevice(_)
                | Call::HostStatus(_)
                | Call::PrepareHostUpdate(_)
                | Call::Invite(_)
                | Call::Revoke(_)
                | Call::ListRemotes(_)
                | Call::RegisterRemote(_)
                | Call::RemoveRemote(_)
        );
        if management {
            let result = if matches!(message, Call::Pair(_)) {
                // Only authorized sessions reach dispatch; the invitation was
                // already consumed at the transport gate.
                Ok(agent_protocol::models::Empty {}.into())
            } else if node != self.local_node && !matches!(message, Call::IdentifyDevice(_)) {
                Err(anyhow::anyhow!("management requires the local node"))
            } else {
                self.manage(node, message).await
            };
            return Ok(Response::from_result(result.map_err(|message| {
                agent_protocol::error::RpcFailure {
                    code: "management_failed".into(),
                    message: format!("{message:#}"),
                    delivery: agent_protocol::error::Delivery::NotSent,
                    execution: None,
                }
            }))
            .into());
        }
        self.service
            .dispatch(session, message)
            .await
            .map_err(anyhow::Error::msg)
    }
    async fn manage(&self, node: NodeId, call: &Call) -> Result<Body> {
        match call {
            Call::IdentifyDevice(params) => {
                let name = params.name.trim();
                anyhow::ensure!(!name.is_empty(), "端末名を入力してください。");
                let mut record = self.credentials.record.lock().await;
                anyhow::ensure!(
                    record.trust.allowed.contains(&node),
                    "peer is not authorized"
                );
                if record.device_names.get(&node).map(String::as_str) != Some(name) {
                    let mut next = record.clone();
                    next.device_names.insert(node, name.to_owned());
                    *record = self.credentials.persist(next).await?;
                }
                drop(record);
                Ok(agent_protocol::operations::HostIdentity {
                    storage_scope: self.service.storage_scope()?,
                    ticket: self.ticket().to_string(),
                    home_directory: directories::BaseDirs::new()
                        .map(|base| base.home_dir().to_string_lossy().into_owned()),
                }
                .into())
            }
            Call::PrepareHostUpdate(_) => {
                let executable = std::env::current_exe()?
                    .into_os_string()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("Host executable path must be UTF-8"))?;
                let arguments = std::env::args_os()
                    .skip(1)
                    .map(|value| {
                        value
                            .into_string()
                            .map_err(|_| anyhow::anyhow!("Host arguments must be UTF-8"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let working_directory = std::env::current_dir()?
                    .into_os_string()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("Host working directory must be UTF-8"))?;
                self.service.prepare_update().await?;
                let stop = self.update_shutdown.clone();
                // Allow the acknowledged preparation response to flush before
                // closing connections. The updater also waits for the lease.
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    stop.cancel();
                });
                Ok(agent_protocol::operations::HostUpdate {
                    executable,
                    arguments,
                    working_directory,
                }
                .into())
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
                        .filter(|id| **id != self.local_node)
                        .map(|id| HostDevice {
                            id: id.to_string(),
                            name: record.device_names.get(id).cloned(),
                        })
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
                next.device_names.remove(&node_id);
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
                let ticket: Ticket = params
                    .ticket
                    .parse()
                    .context("接続情報を読み取れませんでした。")?;
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
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{
        error::{Delivery, PeerError},
        models::Empty,
    };
    use agent_transport::transport::Relays;
    use serde_json::Value;
    use std::path::PathBuf;

    async fn identify(
        runtime: &HostRuntime,
        node: NodeId,
        name: &str,
    ) -> Response<agent_protocol::operations::HostIdentity> {
        let reply = runtime
            .dispatch(
                node,
                0,
                &Call::IdentifyDevice(agent_protocol::operations::IdentifyDevice {
                    name: name.into(),
                }),
            )
            .await
            .unwrap();
        agent_protocol::protocol::decode(&reply.initial).unwrap()
    }

    #[tokio::test]
    async fn device_names_are_owned_by_the_authenticated_node_and_survive_restart() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::FileKeyStore(agent_local_host::identity_path(
            directory.path(),
        )));
        let credentials = Arc::new(
            HostCredentials::load(store.clone(), directory.path().to_owned())
                .await
                .unwrap(),
        );
        let endpoint = Endpoint::bind(credentials.host_identity().await, Relays::Loopback)
            .await
            .unwrap();
        let runtime = HostRuntime::new(
            HostRpcService::new([], directory.path().join("projects.json")),
            endpoint.clone(),
            credentials.clone(),
            "Host".into(),
            Duration::from_secs(60),
        )
        .await;
        let phone = agent_transport::transport::Identity::generate().node_id();
        let tablet = agent_transport::transport::Identity::generate().node_id();
        credentials
            .record
            .lock()
            .await
            .trust
            .allowed
            .extend([phone, tablet]);
        let Response::Success { result } = identify(&runtime, phone, "  My device  ").await else {
            panic!("an authorized device must be identified");
        };
        // The answer carries what the device needs before trusting its cached state.
        assert_eq!(result.ticket, runtime.ticket().to_string());
        assert_eq!(
            result.home_directory,
            directories::BaseDirs::new().map(|base| base.home_dir().to_string_lossy().into_owned())
        );
        assert!(!result.storage_scope.is_empty());
        assert!(matches!(
            identify(&runtime, tablet, "My device").await,
            Response::Success { .. }
        ));
        assert!(matches!(
            identify(&runtime, phone, " \n ").await,
            Response::Failure { .. }
        ));

        let path = directory.path().join("trust.json");
        let backup = directory.path().join("trust-backup.json");
        std::fs::rename(&path, &backup).unwrap();
        std::fs::create_dir(&path).unwrap();
        // Repeating the same name needs no disk write; a failed rename preserves the published name.
        assert!(matches!(
            identify(&runtime, phone, "My device").await,
            Response::Success { .. }
        ));
        assert!(matches!(
            identify(&runtime, phone, "My iPhone").await,
            Response::Failure { .. }
        ));
        assert_eq!(
            credentials.record.lock().await.device_names[&phone],
            "My device"
        );
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(&backup, &path).unwrap();
        assert!(matches!(
            identify(&runtime, phone, "My iPhone").await,
            Response::Success { .. }
        ));

        let restored = Arc::new(
            HostCredentials::load(store.clone(), directory.path().to_owned())
                .await
                .unwrap(),
        );
        let runtime = HostRuntime::new(
            HostRpcService::new([], directory.path().join("projects.json")),
            endpoint.clone(),
            restored,
            "Host".into(),
            Duration::from_secs(60),
        )
        .await;
        let reply = runtime
            .dispatch(runtime.local_node, 0, &Call::HostStatus(Empty {}))
            .await
            .unwrap();
        let Response::Success { result: status } =
            agent_protocol::protocol::decode::<Response<HostStatus>>(&reply.initial).unwrap()
        else {
            panic!("Host status failed")
        };
        assert!(status.devices.contains(&HostDevice {
            id: phone.to_string(),
            name: Some("My iPhone".into())
        }));
        assert!(status.devices.contains(&HostDevice {
            id: tablet.to_string(),
            name: Some("My device".into())
        }));
        assert_eq!(status.devices.len(), 2);
        assert!(
            credentials
                .record
                .lock()
                .await
                .trust
                .allowed
                .contains(&runtime.local_node)
        );

        let reply = runtime
            .dispatch(
                runtime.local_node,
                0,
                &Call::Revoke(agent_protocol::operations::RevokeDevice {
                    id: phone.to_string(),
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            agent_protocol::protocol::decode::<Response<Empty>>(&reply.initial).unwrap(),
            Response::Success { .. }
        ));
        assert!(matches!(
            identify(&runtime, phone, "Revoked").await,
            Response::Failure { .. }
        ));
        let restored = HostCredentials::load(store, directory.path().to_owned())
            .await
            .unwrap();
        let record = restored.record.lock().await;
        assert!(!record.device_names.contains_key(&phone));
        assert_eq!(record.device_names[&tablet], "My device");
        drop(record);
        endpoint.close().await;
    }

    #[tokio::test]
    async fn only_the_local_node_can_prepare_an_app_update() {
        let directory = tempfile::tempdir().unwrap();
        let credentials = Arc::new(
            HostCredentials::load(
                Arc::new(crate::FileKeyStore(agent_local_host::identity_path(
                    directory.path(),
                ))),
                directory.path().to_owned(),
            )
            .await
            .unwrap(),
        );
        let endpoint = Endpoint::bind(credentials.host_identity().await, Relays::Loopback)
            .await
            .unwrap();
        let service = HostRpcService::new([], directory.path().join("projects.json"));
        service.set_history_scope(directory.path().to_owned(), true);
        let runtime = HostRuntime::new(
            service,
            endpoint.clone(),
            credentials,
            "update-host".into(),
            Duration::from_secs(60),
        )
        .await;
        let remote = agent_transport::transport::Identity::generate().node_id();
        let reply = runtime
            .dispatch(
                remote,
                0,
                &Call::PrepareHostUpdate(agent_protocol::operations::PrepareHostUpdate {}),
            )
            .await
            .unwrap();
        let response: Response<agent_protocol::operations::HostUpdate> =
            agent_protocol::protocol::decode(&reply.initial).unwrap();
        assert!(
            matches!(response, Response::Failure { error } if error.code == "management_failed" && error.delivery == Delivery::NotSent)
        );
        assert!(!runtime.update_shutdown.is_cancelled());
        assert!(
            !directory
                .path()
                .join(agent_local_host::update::RECOVERY_FILE)
                .exists()
        );
        let reply = runtime
            .dispatch(
                runtime.local_node,
                0,
                &Call::PrepareHostUpdate(agent_protocol::operations::PrepareHostUpdate {}),
            )
            .await
            .unwrap();
        let response: Response<agent_protocol::operations::HostUpdate> =
            agent_protocol::protocol::decode(&reply.initial).unwrap();
        let Response::Success { result } = response else {
            panic!("local updater could not prepare the Host");
        };
        assert_eq!(
            PathBuf::from(result.executable),
            std::env::current_exe().unwrap()
        );
        assert_eq!(
            result.arguments,
            std::env::args().skip(1).collect::<Vec<_>>()
        );
        assert_eq!(
            PathBuf::from(result.working_directory),
            std::env::current_dir().unwrap()
        );
        endpoint.close().await;
    }

    #[tokio::test]
    async fn an_unsupported_operation_does_not_disconnect_an_authenticated_client() {
        tokio::time::timeout(Duration::from_secs(15), async {
            let directory = tempfile::tempdir().unwrap();
            let credentials = Arc::new(
                HostCredentials::load(
                    Arc::new(crate::FileKeyStore(agent_local_host::identity_path(
                        directory.path(),
                    ))),
                    directory.path().to_owned(),
                )
                .await
                .unwrap(),
            );
            let host = Endpoint::bind(credentials.host_identity().await, Relays::Loopback)
                .await
                .unwrap();
            let client = Endpoint::bind(credentials.local_identity().await, Relays::Loopback)
                .await
                .unwrap();
            let service = HostRpcService::new(
                std::iter::empty::<crate::providers::Backend>(),
                directory.path().join("projects.json"),
            );
            let runtime = Arc::new(
                HostRuntime::new(
                    service,
                    host.clone(),
                    credentials.clone(),
                    "compatibility-host".into(),
                    Duration::from_secs(60),
                )
                .await,
            );
            let ticket = host.ticket();
            let (session, incoming) = tokio::join!(client.connect(&ticket), host.accept());
            let session = session.unwrap();
            let incoming = incoming
                .unwrap()
                .unwrap()
                .authorize(&credentials.record.lock().await.trust)
                .unwrap();
            let (peer, events) = tokio::join!(
                session.open_peer(Duration::from_secs(3), 4),
                incoming.accept_peer()
            );
            let (peer, _updates) = peer.unwrap();
            let events = events.unwrap();
            let serving = tokio::spawn(async move {
                runtime
                    .serve_connection(&incoming, events, CancellationToken::new())
                    .await
            });
            assert_eq!(
                peer.request::<String>(&Call::HostName(Empty {}))
                    .await
                    .unwrap(),
                "compatibility-host"
            );
            // The receiver has no registered method named "Unsupported".
            let error = peer.request::<Empty>(&Call::Unsupported).await.unwrap_err();
            let PeerError::Remote {
                error, delivery, ..
            } = error
            else {
                panic!("expected a remote operation error")
            };
            assert_eq!(delivery, Delivery::NotSent);
            let error: Value = serde_json::from_str(&error).unwrap();
            assert_eq!(error["code"], "method_not_found");
            assert_eq!(
                peer.request::<String>(&Call::HostName(Empty {}))
                    .await
                    .unwrap(),
                "compatibility-host"
            );
            peer.close().await;
            serving.await.unwrap().unwrap();
            session.close();
            client.close().await;
            host.close().await;
        })
        .await
        .unwrap();
    }
}
