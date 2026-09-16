use crate::{HostRuntime, HostSession};
use agent_core::peer::{RpcMessage, RpcMessageKind};
use agent_core::{
    peer::{PeerEvent, RpcPeer},
    transport::NodeId,
};
use std::{sync::Arc, time::Duration};
use tokio::{sync::broadcast, task::JoinSet};
use tokio_util::sync::CancellationToken;

pub(crate) async fn serve_jsonl_session(
    peer: Arc<RpcPeer>,
    mut events: broadcast::Receiver<PeerEvent>,
    runtime: Arc<HostRuntime>,
    node: NodeId,
    stop: CancellationToken,
    mut session: HostSession,
) -> Result<(), String> {
    let id = session.id();
    let mut tasks = JoinSet::<Result<(), String>>::new();
    let result = loop {
        tokio::select! {
            _ = stop.cancelled() => break Ok(()),
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
                let Some(line) = outgoing else { break Err("session outbound queue closed".into()); };
                if let Err(error) = tokio::time::timeout(Duration::from_secs(30), peer.send_raw(line)).await.map_err(|_| "JSONL write timed out".to_owned()).and_then(|r| r.map_err(|e| e.to_string())) {
                    break Err(error);
                }
            }
        }
    };
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    result
}
