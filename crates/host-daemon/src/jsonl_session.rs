use crate::{CodexSession, HostRuntime};
use agent_core::peer::{RpcMessage, RpcMessageKind};
use agent_core::{
    peer::{PeerEvent, RpcPeer},
    transport::NodeId,
};
use futures_util::{
    StreamExt,
    future::{AbortHandle, abortable},
    stream::FuturesUnordered,
};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{sync::broadcast, task::JoinSet};
use tokio_util::sync::CancellationToken;

pub(crate) async fn serve_jsonl_session(
    peer: Arc<RpcPeer>,
    mut events: broadcast::Receiver<PeerEvent>,
    runtime: Arc<HostRuntime>,
    node: NodeId,
    stop: CancellationToken,
    mut session: CodexSession,
) -> Result<(), String> {
    let id = session.id();
    let mut tasks = JoinSet::<Result<(), String>>::new();
    let mut requests = FuturesUnordered::new();
    let mut aliases = HashMap::<String, (u64, AbortHandle)>::new();
    let result = loop {
        tokio::select! {
            _ = stop.cancelled() => break Ok(()),
            completed = requests.next(), if !requests.is_empty() => {
                if let Some(Ok(Err(error))) = completed { break Err(error); }
            },
            completed = tasks.join_next(), if !tasks.is_empty() => match completed {
                Some(Ok(Ok(()))) => {},
                Some(Ok(Err(error))) => break Err(error),
                Some(Err(error)) => break Err(error.to_string()),
                None => {},
            },
            incoming = events.recv() => match incoming {
                Ok(PeerEvent::Response { .. }) => {},
                Ok(PeerEvent::Closed(_)) => break Ok(()),
                Err(error) => break Err(error.to_string()),
                Ok(PeerEvent::Message(message)) => {
                    let parsed = RpcMessage::parse(&message.value).map_err(|e| e.to_string())?;
                    if parsed.kind() == RpcMessageKind::Notification {
                        runtime.dispatch(node, id, &parsed).await?;
                    } else {
                        if tasks.len() >= 128 { break Err("maximum in-flight request count reached".into()); }
                        let peer = peer.clone();
                        let runtime = runtime.clone();
                        tasks.spawn(async move {
                            if let Some(response) = runtime.dispatch(node, id, &RpcMessage::parse(&message.value).map_err(|e| e.to_string())?).await? {
                                peer.send_raw(response).await.map_err(|e| e.to_string())?;
                            }
                            Ok(())
                        });
                    }
                },
            },
            outgoing = session.recv() => {
                let Some(mut line) = outgoing else { break Err("session outbound queue closed".into()); };
                let message = match RpcMessage::parse(&line) {
                    Ok(message) => message,
                    Err(error) => break Err(error.to_string()),
                };
                if message.kind() == RpcMessageKind::Request {
                    let alias = message.raw_id().ok_or("server request ID missing")?.to_owned();
                    let request = peer.request_raw(&line);
                    let wire_id = request.wire_id().ok_or("invalid server request")?;
                    let service = &runtime.service;
                    let (request, abort) = abortable(async move {
                        let response = request.await.map_err(|e| e.to_string())?;
                        service.dispatch(id, &RpcMessage::parse(&response.value).map_err(|e| e.to_string())?).await?;
                        Ok::<_, String>(())
                    });
                    if let Some((_, previous)) = aliases.insert(alias, (wire_id, abort)) { previous.abort(); }
                    requests.push(request);
                } else {
                    if message.method() == Some("serverRequest/resolved") {
                        let mut notification: serde_json::Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
                        if let Some(request_id) = notification.get_mut("params").and_then(|params| params.get_mut("requestId"))
                            && let Some((wire_id, abort)) = aliases.remove(&request_id.to_string()) {
                            abort.abort();
                            *request_id = wire_id.into();
                            line = notification.to_string();
                        }
                    }
                    if let Err(error) = tokio::time::timeout(Duration::from_secs(30), peer.send_raw(line)).await.map_err(|_| "JSONL write timed out".to_owned()).and_then(|r| r.map_err(|e| e.to_string())) {
                        break Err(error);
                    }
                }
            }
        }
    };
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    result
}
