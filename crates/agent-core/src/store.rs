//! The single state owner. RPC work runs concurrently; publication follows wire order.
use crate::{
    client::*,
    peer::{PeerError, PeerEvent, RpcPeer},
    state::{Effect, Event, Intent, Snapshot, reduce},
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use host_protocol::{RpcMessageKind, classify_message};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, RwLock},
};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_util::sync::{CancellationToken, DropGuard};

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Applied,
    StartedThread(String),
    Submitted(Option<String>),
}
struct Dispatch {
    intent: Intent,
    complete: oneshot::Sender<Result<Outcome, PeerError>>,
}
struct Applied {
    sequence: Option<u64>,
    event: Option<Event>,
    outcome: Outcome,
}
struct Completed {
    result: Result<Applied, PeerError>,
    ordered: bool,
    complete: Option<oneshot::Sender<Result<Outcome, PeerError>>>,
}
pub struct Store {
    snapshot: Arc<RwLock<Arc<Snapshot>>>,
    updates: watch::Sender<Arc<Snapshot>>,
    commands: mpsc::Sender<Dispatch>,
    stop: CancellationToken,
    _close_on_drop: DropGuard,
    session: Option<crate::transport::Session>,
    finished: watch::Receiver<Option<Result<(), String>>>,
}
impl Store {
    pub fn new(peer: RpcPeer, snapshot: Snapshot) -> Self {
        let snapshot = Arc::new(RwLock::new(Arc::new(snapshot)));
        let (updates, _) = watch::channel(snapshot.read().unwrap().clone());
        let (commands, incoming) = mpsc::channel(256);
        let stop = CancellationToken::new();
        let (finished_tx, finished) = watch::channel(None);
        let run = run(
            peer,
            snapshot.clone(),
            updates.clone(),
            incoming,
            stop.clone(),
        );
        tokio::spawn(async move {
            finished_tx.send_replace(Some(run.await.map_err(|error| error.to_string())));
        });
        Self {
            snapshot,
            updates,
            commands,
            _close_on_drop: stop.clone().drop_guard(),
            stop,
            session: None,
            finished,
        }
    }
    pub async fn connect(
        session: crate::transport::Session,
        snapshot: Snapshot,
    ) -> Result<Self, crate::transport::TransportError> {
        let peer = session
            .open_peer(std::time::Duration::from_secs(30), 64)
            .await?;
        let mut store = Self::new(peer, snapshot);
        store.session = Some(session);
        Ok(store)
    }
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.read().unwrap().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<Arc<Snapshot>> {
        self.updates.subscribe()
    }
    pub async fn dispatch(&self, intent: Intent) -> Result<Outcome, PeerError> {
        let (complete, result) = oneshot::channel();
        self.commands
            .send(Dispatch { intent, complete })
            .await
            .map_err(|_| PeerError::ConnectionClosed("store is closed".into()))?;
        result
            .await
            .map_err(|_| PeerError::ConnectionClosed("store is closed".into()))?
    }
    pub async fn close(&self) -> Result<(), PeerError> {
        self.stop.cancel();
        let mut finished = self.finished.clone();
        let result = loop {
            if let Some(result) = finished.borrow_and_update().clone() {
                break result.map_err(PeerError::ConnectionClosed);
            }
            finished
                .changed()
                .await
                .map_err(|_| PeerError::ConnectionClosed("store task stopped".into()))?;
        };
        if let Some(session) = &self.session {
            session.close();
        }
        result
    }
}

fn apply(
    snapshot: &RwLock<Arc<Snapshot>>,
    updates: &watch::Sender<Arc<Snapshot>>,
    event: Event,
) -> Vec<Effect> {
    let mut current = snapshot.write().unwrap();
    let (next, effects) = reduce(&current, event);
    let same_threads = match (&current.threads, &next.threads) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    };
    if Arc::ptr_eq(&current.conversations, &next.conversations)
        && same_threads
        && Arc::ptr_eq(&current.models, &next.models)
        && Arc::ptr_eq(&current.requests, &next.requests)
        && Arc::ptr_eq(&current.drafts, &next.drafts)
        && current.connected == next.connected
        && current.error == next.error
    {
        return effects;
    }
    *current = Arc::new(next);
    updates.send_replace(current.clone());
    effects
}
fn needs_order(method: &str) -> bool {
    [
        ListThreads::METHOD,
        StartThread::METHOD,
        ReadThread::METHOD,
        OlderTurns::METHOD,
        OlderItems::METHOD,
        ReadItem::METHOD,
    ]
    .contains(&method)
}
fn completion_sequence(completed: &Completed) -> Option<u64> {
    if !completed.ordered {
        return None;
    }
    match &completed.result {
        Ok(applied) => applied.sequence,
        Err(PeerError::Remote { sequence, .. } | PeerError::InvalidResponse { sequence, .. }) => {
            *sequence
        }
        _ => None,
    }
}
fn finish(
    snapshot: &RwLock<Arc<Snapshot>>,
    updates: &watch::Sender<Arc<Snapshot>>,
    completed: Completed,
) -> Vec<Effect> {
    let mut effects = Vec::new();
    let result = match completed.result {
        Ok(applied) => {
            if let Some(event) = applied.event {
                effects = apply(snapshot, updates, event);
            }
            Ok(applied.outcome)
        }
        Err(error) => {
            effects = apply(snapshot, updates, Event::Failed(error.to_string()));
            Err(error)
        }
    };
    if let Some(complete) = completed.complete {
        let _ = complete.send(result);
    }
    effects
}
fn decode_message(line: &str) -> Result<Event, PeerError> {
    let message =
        classify_message(line).map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
    if message.kind() == RpcMessageKind::Request {
        return serde_json::from_str(line)
            .map(Event::ServerRequest)
            .map_err(|error| PeerError::InvalidMessage(error.to_string()));
    }
    #[derive(serde::Deserialize)]
    struct Notification {
        method: String,
        #[serde(default)]
        params: serde_json::Value,
    }
    let notification: Notification =
        serde_json::from_str(line).map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
    Ok(Event::Notification {
        method: notification.method,
        params: notification.params,
    })
}
async fn run(
    peer: RpcPeer,
    snapshot: Arc<RwLock<Arc<Snapshot>>>,
    updates: watch::Sender<Arc<Snapshot>>,
    mut commands: mpsc::Receiver<Dispatch>,
    stop: CancellationToken,
) -> Result<(), PeerError> {
    let mut events = peer.subscribe();
    let peer = Arc::new(peer);
    let client = Arc::new(Client::new(peer.clone()));
    let mut jobs = FuturesUnordered::new();
    let mut received = VecDeque::new();
    let mut completed = BTreeMap::new();
    let mut stream_open = true;
    let mut terminal_reason = None;
    let mut effects = apply(&snapshot, &updates, Event::Connected);
    let reason = loop {
        for Effect::Execute(intent) in effects.drain(..) {
            jobs.push(perform(
                client.clone(),
                snapshot.read().unwrap().clone(),
                intent,
                None,
            ));
        }
        if !stream_open && jobs.is_empty() && completed.is_empty() && received.is_empty() {
            break terminal_reason.unwrap_or_else(|| "RPC stream closed".into());
        }
        tokio::select! {
            _ = stop.cancelled() => break "store closed".into(),
            command = commands.recv() => {
                let Some(command) = command else { break "store closed".into() };
                let pending = apply(&snapshot,&updates,Event::Intent(command.intent));
                let mut complete = Some(command.complete);
                if pending.is_empty() { let _ = complete.take().unwrap().send(Ok(Outcome::Applied)); }
                for Effect::Execute(intent) in pending {
                    jobs.push(perform(client.clone(),snapshot.read().unwrap().clone(),intent,complete.take()));
                }
            }
            result = jobs.next(), if !jobs.is_empty() => {
                let result = result.unwrap();
                if let Some(sequence) = completion_sequence(&result) { completed.insert(sequence,result); }
                else { effects.extend(finish(&snapshot,&updates,result)); }
            }
            event = events.recv(), if stream_open => match event {
                Ok(event) => received.push_back(event),
                Err(broadcast::error::RecvError::Lagged(count)) => break format!("lost {count} RPC events; reconnect and reload state"),
                Err(broadcast::error::RecvError::Closed) => stream_open = false,
            }
        }
        while let Some(event) = received.front() {
            if let PeerEvent::Response {
                sequence,
                method: Some(method),
            } = event
                && needs_order(method)
            {
                let Some(result) = completed.remove(sequence) else {
                    break;
                };
                effects.extend(finish(&snapshot, &updates, result));
            }
            match received.pop_front().unwrap() {
                PeerEvent::Message(frame) => {
                    let event = decode_message(&frame.value)
                        .unwrap_or_else(|error| Event::Failed(error.to_string()));
                    effects.extend(apply(&snapshot, &updates, event));
                }
                PeerEvent::Closed(reason) => {
                    terminal_reason = Some(reason);
                }
                PeerEvent::Response { .. } => {}
            }
        }
    };
    let result = peer.close().await;
    apply(&snapshot, &updates, Event::Disconnected(reason));
    result
}

async fn perform(
    client: Arc<Client>,
    snapshot: Arc<Snapshot>,
    intent: Intent,
    complete: Option<oneshot::Sender<Result<Outcome, PeerError>>>,
) -> Completed {
    let ordered = matches!(
        intent,
        Intent::ListThreads(_)
            | Intent::StartThread { .. }
            | Intent::ReadThread(_)
            | Intent::ReadOlder { .. }
            | Intent::ReadItem { .. }
    );
    let result = async {
        let applied = match intent {
            Intent::ListThreads(query) => {
                let reply = client
                    .call(&ListThreads {
                        title_only: true,
                        query: &query,
                    })
                    .await?;
                Applied {
                    sequence: Some(reply.sequence),
                    event: Some(Event::ThreadsLoaded(reply.value)),
                    outcome: Outcome::Applied,
                }
            }
            Intent::StartThread { cwd, model } => {
                let reply = client
                    .call(&StartThread {
                        cwd: cwd.as_deref().filter(|cwd| !cwd.trim().is_empty()),
                        model: model.as_deref(),
                    })
                    .await?;
                let id = reply.value.thread.id.clone().expect("validated thread ID");
                Applied {
                    sequence: Some(reply.sequence),
                    event: Some(Event::ThreadRefreshed(reply.value.thread)),
                    outcome: Outcome::StartedThread(id),
                }
            }
            Intent::ReadThread(thread_id) => {
                let reply = client
                    .call(&ReadThread {
                        thread_id: &thread_id,
                        include_turns: true,
                        paginate_history: true,
                        defer_item_details: true,
                    })
                    .await?;
                Applied {
                    sequence: Some(reply.sequence),
                    event: Some(Event::ThreadRefreshed(reply.value.thread)),
                    outcome: Outcome::Applied,
                }
            }
            Intent::ReadOlder {
                thread_id,
                turn_id,
                cursor,
            } => {
                let reply = if let Some(turn_id) = &turn_id {
                    client
                        .call(&OlderItems {
                            thread_id: &thread_id,
                            turn_id,
                            cursor: cursor.as_deref(),
                            defer_item_details: true,
                        })
                        .await?
                } else {
                    client
                        .call(&OlderTurns {
                            thread_id: &thread_id,
                            turn_id: None,
                            cursor: cursor.as_deref(),
                            defer_item_details: true,
                        })
                        .await?
                };
                Applied {
                    sequence: Some(reply.sequence),
                    event: Some(Event::OlderLoaded {
                        thread_id,
                        thread: reply.value.thread,
                        turn_id,
                        cursor,
                    }),
                    outcome: Outcome::Applied,
                }
            }
            Intent::ReadItem {
                thread_id,
                turn_id,
                item_id,
            } => {
                let reply = client
                    .call(&ReadItem {
                        thread_id: &thread_id,
                        turn_id: &turn_id,
                        item_id: &item_id,
                    })
                    .await?;
                Applied {
                    sequence: Some(reply.sequence),
                    event: Some(Event::ItemLoaded {
                        thread_id,
                        turn_id,
                        item: reply.value.item,
                    }),
                    outcome: Outcome::Applied,
                }
            }
            Intent::LoadModels => Applied {
                sequence: None,
                event: Some(Event::ModelsLoaded(client.models().await?)),
                outcome: Outcome::Applied,
            },
            Intent::Submit {
                thread_id,
                client_user_message_id,
            } => {
                let draft = snapshot.drafts.get(&thread_id).cloned().unwrap_or_default();
                let target = submission_target(
                    snapshot.conversations.get(&thread_id).map(Arc::as_ref),
                    snapshot.threads.as_ref().and_then(|list| {
                        list.data
                            .iter()
                            .find(|thread| thread.id.as_ref() == Some(&thread_id))
                    }),
                )?;
                let mut input = Vec::with_capacity(
                    draft.attachments.len() + usize::from(!draft.text.is_empty()),
                );
                if !draft.text.is_empty() {
                    input.push(Input::Text {
                        text: &draft.text,
                        text_elements: &[],
                    });
                }
                for attachment in &draft.attachments {
                    input.push(if attachment.is_image {
                        Input::LocalImage {
                            path: &attachment.path,
                        }
                    } else {
                        Input::Mention {
                            path: &attachment.path,
                            name: &attachment.name,
                        }
                    });
                }
                let reply = client
                    .submit(
                        &Submission {
                            thread_id: &thread_id,
                            client_user_message_id: &client_user_message_id,
                            input: &input,
                            model: draft.model.as_deref(),
                            effort: draft.effort.as_deref(),
                        },
                        target,
                    )
                    .await?;
                Applied {
                    sequence: Some(reply.sequence),
                    event: Some(Event::Submitted { thread_id, draft }),
                    outcome: Outcome::Submitted(reply.value),
                }
            }
            Intent::Interrupt { thread_id, turn_id } => {
                client
                    .call(&InterruptTurn {
                        thread_id: &thread_id,
                        turn_id: &turn_id,
                    })
                    .await?;
                Applied {
                    sequence: None,
                    event: None,
                    outcome: Outcome::Applied,
                }
            }
            Intent::Respond { request_id, answer } => {
                let request = snapshot
                    .requests
                    .get(&request_id.to_string())
                    .ok_or_else(|| {
                        PeerError::InvalidMessage("server request is no longer pending".into())
                    })?;
                client.respond(request, &answer).await?;
                Applied {
                    sequence: None,
                    event: Some(Event::RequestResolved(request_id)),
                    outcome: Outcome::Applied,
                }
            }
            Intent::Watch {
                thread_id,
                watch_key,
                watch_id,
                path,
            } => {
                client
                    .call(&WatchThread {
                        thread_id: &thread_id,
                        watch_key,
                        watch_id,
                        path: path.as_deref(),
                    })
                    .await?;
                Applied {
                    sequence: None,
                    event: None,
                    outcome: Outcome::Applied,
                }
            }
            Intent::Unwatch {
                watch_key,
                watch_id,
            } => {
                client
                    .call(&UnwatchThread {
                        watch_key,
                        watch_id,
                    })
                    .await?;
                Applied {
                    sequence: None,
                    event: None,
                    outcome: Outcome::Applied,
                }
            }
            intent @ Intent::SetDraft { .. } => Applied {
                sequence: None,
                event: Some(Event::Intent(intent)),
                outcome: Outcome::Applied,
            },
        };
        Ok(applied)
    }
    .await;
    Completed {
        result,
        ordered,
        complete,
    }
}
