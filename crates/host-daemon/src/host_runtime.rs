use crate::{CodexRpcService, HostCredentials, RemoteHostProfile, SessionId};
use agent_core::{
    peer::{PeerEvent, RpcPeer},
    transport::{Endpoint, IncomingSession, NodeId, PairingTicket, Session, Ticket, authorize},
};
use host_protocol::{RpcMessageKind, classify_message};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub struct HostRuntime {
    pub(crate) service: CodexRpcService,
    endpoint: Endpoint,
    credentials: Arc<HostCredentials>,
    local_node: NodeId,
    name: String,
    active: Mutex<BTreeMap<SessionId, Session>>,
}
impl HostRuntime {
    pub async fn new(
        service: CodexRpcService,
        endpoint: Endpoint,
        credentials: Arc<HostCredentials>,
        name: String,
    ) -> Self {
        let local_node = credentials.local_identity().await.node_id();
        Self {
            service,
            endpoint,
            credentials,
            local_node,
            name,
            active: Mutex::new(BTreeMap::new()),
        }
    }
    pub fn ticket(&self) -> Ticket {
        self.endpoint.ticket()
    }
    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) -> Result<(), String> {
        self.service.start();
        let mut sessions = JoinSet::new();
        let authorized_slots = Arc::new(tokio::sync::Semaphore::new(64));
        let pairing_slots = Arc::new(tokio::sync::Semaphore::new(16));
        let result = loop {
            tokio::select! {
                _ = shutdown.cancelled() => break Ok(()),
                _ = self.service.stopped() => break Err("Codex App Server event stream stopped".into()),
                incoming = self.endpoint.accept() => match incoming {
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
                    Some(Err(_)) => continue,
                    None => break Ok(()),
                },
                Some(_) = sessions.join_next(), if !sessions.is_empty() => {}
            }
        };
        shutdown.cancel();
        while sessions.join_next().await.is_some() {}
        self.endpoint.close().await;
        result
    }
    async fn serve(
        self: Arc<Self>,
        incoming: IncomingSession,
        stop: CancellationToken,
        permit: tokio::sync::OwnedSemaphorePermit,
        authorized_slots: Arc<tokio::sync::Semaphore>,
    ) -> Result<(), String> {
        let node = incoming.node_id();
        let establish = async {
            let record = self.credentials.record.lock().await;
            if record.trust.allowed.contains(&node) {
                let connection = scopeguard::guard(
                    incoming
                        .authorize(&record.trust)
                        .map_err(|e| e.to_string())?,
                    |session| session.close(),
                );
                drop(record);
                let stream = connection
                    .accept_stream()
                    .await
                    .map_err(|e| e.to_string())?;
                let (read, write) = tokio::io::split(stream);
                let peer = RpcPeer::open(host_protocol::JsonlReader::new(read), write, None, 128)
                    .map_err(|e| e.to_string())?;
                let events = peer.subscribe();
                Ok((scopeguard::ScopeGuard::into_inner(connection), peer, events))
            } else {
                drop(record);
                let pairing = incoming.pairing().await.map_err(|e| e.to_string())?;
                self.pair_node(node, pairing.invitation).await?;
                let trust = self.credentials.record.lock().await.trust.clone();
                pairing.authorize(&trust).await.map_err(|e| e.to_string())
            }
        };
        let (connection, peer, events) = tokio::select! {
            _ = stop.cancelled() => return Ok(()),
            result = tokio::time::timeout(Duration::from_secs(15), establish) =>
                result.map_err(|_| "session establishment timed out")??,
        };
        let connection = scopeguard::guard(connection, |session| session.close());
        let _permit = if Arc::ptr_eq(permit.semaphore(), &authorized_slots) {
            permit
        } else {
            // Paired sessions move out of the pre-authorization pool.
            let authorized = authorized_slots
                .try_acquire_owned()
                .map_err(|_| "maximum authorized sessions reached")?;
            drop(permit);
            authorized
        };
        self.serve_connection(&connection, Arc::new(peer), events, stop)
            .await
    }
    async fn serve_connection(
        self: Arc<Self>,
        connection: &Session,
        peer: Arc<RpcPeer>,
        events: tokio::sync::broadcast::Receiver<PeerEvent>,
        stop: CancellationToken,
    ) -> Result<(), String> {
        let node = connection.node_id();
        let session = {
            // Registration and revocation use the same lock order. A revoked node
            // cannot become active in the gap after initial authentication.
            let record = self.credentials.record.lock().await;
            if !record.trust.allowed.contains(&node) {
                return Err("peer is not authorized".into());
            }
            let session = self.service.open_session(128);
            self.active
                .lock()
                .unwrap()
                .insert(session.id(), connection.clone());
            session
        };
        let id = session.id();
        let rpc = crate::jsonl_session::serve_jsonl_session(
            peer.clone(),
            events,
            self.clone(),
            node,
            stop.clone(),
            session,
        );
        tokio::pin!(rpc);
        let mut transfers = JoinSet::new();
        let mut rpc_finished = false;
        let result = loop {
            tokio::select! {
                result = &mut rpc => { rpc_finished = true; break result; },
                stream = connection.accept_stream() => match stream {
                    Ok(stream) => {
                        if transfers.len() >= 16 { continue; }
                        let service = self.service.clone();
                        transfers.spawn(async move { service.files().transfer(id, stream).await });
                    }
                    Err(error) => break Err(error.to_string()),
                },
                Some(_) = transfers.join_next(), if !transfers.is_empty() => {}
            }
        };
        stop.cancel();
        let rpc_result = if rpc_finished { Ok(()) } else { rpc.await };
        transfers.abort_all();
        while transfers.join_next().await.is_some() {}
        self.active.lock().unwrap().remove(&id);
        self.service.close_session(id);
        let closed = peer.close().await.map_err(|e| e.to_string());
        result.and(rpc_result).and(closed)
    }
    async fn pair_node(&self, node: NodeId, invitation: uuid::Uuid) -> Result<(), String> {
        let mut record = self.credentials.record.lock().await;
        if let Some(trust) =
            authorize(&record.trust, node, Some(invitation), now()).map_err(|e| e.to_string())?
        {
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
        line: String,
    ) -> Result<Option<String>, String> {
        let message = classify_message(&line).map_err(|e| e.to_string())?;
        let management = matches!(
            message.method(),
            Some(
                "host/pair"
                    | "host/status"
                    | "host/invite"
                    | "host/revoke"
                    | "host/listRemotes"
                    | "host/registerRemote"
                    | "host/removeRemote"
            )
        );
        if management && message.kind() == RpcMessageKind::Request {
            let request: Request = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            let result = if request.method == "host/pair" {
                // Only authorized sessions reach dispatch; the invitation was
                // already consumed at the transport gate.
                Ok(json!({}))
            } else if node != self.local_node {
                Err("management requires the local node".into())
            } else {
                self.manage(&request.method, request.params).await
            };
            return Ok(Some(response(request.id, result)));
        }
        match message.kind() {
            RpcMessageKind::Request => self
                .service
                .dispatch_request(session, line)
                .await
                .map_err(|e| e.to_string())?,
            RpcMessageKind::Notification => self
                .service
                .dispatch_notification(session, line)
                .await
                .map_err(|e| e.to_string())?,
            RpcMessageKind::Response => {
                self.service
                    .dispatch_response(session, line)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(None)
    }
    async fn manage(&self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "host/status" => {
                let record = self.credentials.record.lock().await;
                Ok(
                    json!({"nodeId":self.endpoint.node_id(), "name":self.name, "devices":record.trust.allowed}),
                )
            }
            "host/invite" => {
                let ticket = PairingTicket::new(self.endpoint.ticket(), now() + 300);
                let mut record = self.credentials.record.lock().await;
                let mut next = record.clone();
                next.trust.invitations.retain(|_, expiry| now() < *expiry);
                next.trust
                    .invitations
                    .insert(ticket.invitation, ticket.expires_at);
                *record = self.credentials.persist(next).await?;
                serde_json::to_value(ticket).map_err(|e| e.to_string())
            }
            "host/revoke" => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Revoke {
                    node_id: NodeId,
                }
                let params: Revoke =
                    serde_json::from_value(params).map_err(|_| "invalid node ID")?;
                if params.node_id == self.local_node {
                    return Err("cannot revoke the local node".into());
                }
                let mut record = self.credentials.record.lock().await;
                let mut next = record.clone();
                next.trust.allowed.remove(&params.node_id);
                *record = self.credentials.persist(next).await?;
                for connection in self.active.lock().unwrap().values() {
                    if connection.node_id() == params.node_id {
                        connection.close();
                    }
                }
                Ok(json!({}))
            }
            "host/listRemotes" => serde_json::to_value(
                self.credentials
                    .record
                    .lock()
                    .await
                    .remotes
                    .values()
                    .collect::<Vec<_>>(),
            )
            .map_err(|e| e.to_string()),
            "host/registerRemote" => {
                #[derive(Deserialize)]
                struct Register {
                    ticket: Ticket,
                    name: String,
                }
                let params: Register =
                    serde_json::from_value(params).map_err(|_| "invalid remote profile")?;
                // Pairing runs on the client's existing endpoint. The Host only
                // persists its local client's destination; it never binds that key.
                let profile = RemoteHostProfile {
                    id: params.ticket.node_id(),
                    name: params.name,
                    ticket: params.ticket,
                };
                let mut record = self.credentials.record.lock().await;
                let mut next = record.clone();
                next.remotes.insert(profile.id, profile.clone());
                *record = self.credentials.persist(next).await?;
                serde_json::to_value(profile).map_err(|e| e.to_string())
            }
            "host/removeRemote" => {
                #[derive(Deserialize)]
                struct Remove {
                    id: NodeId,
                }
                let params: Remove =
                    serde_json::from_value(params).map_err(|_| "invalid remote ID")?;
                let mut record = self.credentials.record.lock().await;
                let mut next = record.clone();
                next.remotes.remove(&params.id);
                *record = self.credentials.persist(next).await?;
                Ok(json!({}))
            }
            _ => Err("unknown management method".into()),
        }
    }
}
#[derive(Deserialize)]
struct Request {
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}
fn response(id: Value, result: Result<Value, String>) -> String {
    match result {
        Ok(result) => json!({"id":id,"result":result}),
        Err(message) => json!({"id":id,"error":{"code":-32602,"message":message}}),
    }
    .to_string()
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
