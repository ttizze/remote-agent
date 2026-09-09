use crate::{CodexRpcService, HostCredentials, SessionId};
use agent_core::{
    models::{HostStatus, Invitation, RemoteHost},
    peer::{PeerEvent, RpcPeer},
    transport::{Endpoint, NodeId, Relays, Session, Ticket, authorize},
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
    relays: Relays,
    active: Mutex<BTreeMap<SessionId, Session>>,
}
impl HostRuntime {
    pub async fn new(
        service: CodexRpcService,
        endpoint: Endpoint,
        credentials: Arc<HostCredentials>,
        name: String,
        relays: Relays,
    ) -> Self {
        let local_node = credentials.local_identity().await.node_id();
        Self {
            service,
            endpoint,
            credentials,
            local_node,
            name,
            relays,
            active: Mutex::new(BTreeMap::new()),
        }
    }
    pub fn ticket(&self) -> Ticket {
        self.endpoint.ticket()
    }
    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) -> Result<(), String> {
        let mut sessions = JoinSet::new();
        let result = loop {
            tokio::select! {
                _ = shutdown.cancelled() => break Ok(()),
                incoming = self.endpoint.accept() => match incoming {
                    Ok(session) => {
                        if sessions.len() >= 64 { session.close(); continue; }
                        let runtime = self.clone();
                        let stop = shutdown.child_token();
                        sessions.spawn(async move { runtime.serve(session, stop).await });
                    }
                    Err(error) => break Err(error.to_string()),
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
        connection: Session,
        stop: CancellationToken,
    ) -> Result<(), String> {
        let result = self.clone().serve_connection(&connection, stop).await;
        connection.close();
        result
    }
    async fn serve_connection(
        self: Arc<Self>,
        connection: &Session,
        stop: CancellationToken,
    ) -> Result<(), String> {
        let stream = tokio::select! {
            _ = stop.cancelled() => return Ok(()),
            result = tokio::time::timeout(Duration::from_secs(15), connection.accept_stream()) =>
                result.map_err(|_| "first stream timed out")?.map_err(|e| e.to_string())?,
        };
        let (read, write) = tokio::io::split(stream);
        let peer = Arc::new(
            RpcPeer::open(
                host_protocol::JsonlReader::new(read),
                write,
                Duration::from_secs(300),
                128,
            )
            .map_err(|e| e.to_string())?,
        );
        let mut events = peer.subscribe();
        let node = connection.node_id();
        if !self
            .credentials
            .record
            .lock()
            .await
            .trust
            .allowed
            .contains(&node)
        {
            let message = tokio::select! {
                _ = stop.cancelled() => return Ok(()),
                event = tokio::time::timeout(Duration::from_secs(15), events.recv()) =>
                    event.map_err(|_| "pairing timed out")?.map_err(|_| "pairing connection closed")?,
            };
            let PeerEvent::Message(message) = message else {
                return Err("pairing request required".into());
            };
            let request: Request =
                serde_json::from_str(&message.value).map_err(|_| "invalid pairing request")?;
            if request.method != "host/pair" {
                return Err("pairing request required".into());
            }
            let result = self.pair_node(node, request.params).await;
            peer.send_raw(response(
                request.id,
                result.as_ref().map(|_| json!({})).map_err(Clone::clone),
            ))
            .await
            .map_err(|e| e.to_string())?;
            result?;
        }
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
    async fn pair_node(&self, node: NodeId, params: Value) -> Result<(), String> {
        #[derive(Deserialize)]
        struct Pair {
            invitation: uuid::Uuid,
        }
        let params: Pair = serde_json::from_value(params).map_err(|_| "invalid invitation")?;
        let mut record = self.credentials.record.lock().await;
        if let Some(trust) = authorize(&record.trust, node, Some(params.invitation), now())
            .map_err(|e| e.to_string())?
        {
            let mut next = record.clone();
            next.trust = trust;
            self.credentials.persist(&next)?;
            *record = next;
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
                    | "host/pairRemote"
                    | "host/removeRemote"
            )
        );
        if management && message.kind() == RpcMessageKind::Request {
            let request: Request = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            let result = if request.method == "host/pair" {
                self.pair_node(node, request.params)
                    .await
                    .map(|_| json!({}))
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
                serde_json::to_value(HostStatus {
                    node_id: self.endpoint.node_id().to_string(),
                    name: self.name.clone(),
                    devices: record
                        .trust
                        .allowed
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                    extra: Default::default(),
                })
                .map_err(|error| error.to_string())
            }
            "host/invite" => {
                let ticket = Invitation {
                    endpoint: self.endpoint.ticket().to_string(),
                    invitation: uuid::Uuid::new_v4(),
                    expires_at: now() + 300,
                    extra: Default::default(),
                };
                let mut record = self.credentials.record.lock().await;
                let mut next = record.clone();
                next.trust.invitations.retain(|_, expiry| now() < *expiry);
                next.trust
                    .invitations
                    .insert(ticket.invitation, ticket.expires_at);
                self.credentials.persist(&next)?;
                *record = next;
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
                self.credentials.persist(&next)?;
                *record = next;
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
            "host/pairRemote" => {
                #[derive(Deserialize)]
                struct Pair {
                    invitation: Invitation,
                    name: String,
                }
                let params: Pair =
                    serde_json::from_value(params).map_err(|_| "invalid remote invitation")?;
                if now() >= params.invitation.expires_at {
                    return Err("invitation expired".into());
                }
                let ticket: Ticket =
                    params.invitation.endpoint.parse().map_err(
                        |error: agent_core::transport::TransportError| error.to_string(),
                    )?;
                // Pair the desktop's durable identity, which will connect directly.
                let endpoint =
                    Endpoint::bind(self.credentials.local_identity().await, self.relays.clone())
                        .await
                        .map_err(|e| e.to_string())?;
                let connection = endpoint.connect(&ticket).await.map_err(|e| e.to_string())?;
                let peer = connection
                    .open_peer(Duration::from_secs(20), 8)
                    .await
                    .map_err(|e| e.to_string())?;
                let result = peer
                    .request::<_, Value>(
                        "host/pair",
                        &json!({"invitation":params.invitation.invitation}),
                    )
                    .await;
                let closed = peer.close().await;
                connection.close();
                endpoint.close().await;
                result.map_err(|e| e.to_string())?;
                closed.map_err(|e| e.to_string())?;
                let node_id = ticket.node_id();
                let profile = RemoteHost {
                    id: node_id.to_string(),
                    name: params.name,
                    ticket: params.invitation.endpoint,
                    extra: Default::default(),
                };
                let mut record = self.credentials.record.lock().await;
                let mut next = record.clone();
                next.remotes.insert(node_id, profile.clone());
                self.credentials.persist(&next)?;
                *record = next;
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
                self.credentials.persist(&next)?;
                *record = next;
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
