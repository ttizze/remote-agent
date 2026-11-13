//! Each conversation has one receiver. It commits decisions before publishing
//! rows or asking the resource executor to perform an action.
use super::routing::{MAX_QUEUED_BYTES, frame_cost};
use super::storage::{Committed, Storage};
use agent_protocol::{
    conversation::{Action, Context, Event, Outcome, records::Message},
    protocol,
    session::SessionRef,
    sync::{Cursor, Scope, Window},
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicUsize, Ordering::Relaxed},
    },
};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

const QUEUED_MESSAGES: usize = 1000;
const STREAM_QUEUED_BYTES: usize = 8 * 1024 * 1024;

pub(super) struct Owners {
    storage: Arc<Storage>,
    actors: Mutex<HashMap<SessionRef, (Uuid, mpsc::UnboundedSender<Input>)>>,
    effects: mpsc::UnboundedSender<Effect>,
}

pub(super) struct Effect {
    pub target: SessionRef,
    pub action: Action,
}

type WriteResult = Result<(Outcome, Cursor), String>;

enum Input {
    Write {
        write: Box<Write>,
        reply: oneshot::Sender<WriteResult>,
    },
    Subscribe {
        session: u64,
        bytes: Arc<AtomicUsize>,
        subscription: Uuid,
        cached: Option<Cursor>,
        window: Option<Window>,
        reply: oneshot::Sender<Result<Opened, String>>,
    },
    Read {
        session: u64,
        subscription: Uuid,
        item: Option<agent_protocol::ids::ItemId>,
        reply: oneshot::Sender<Result<Read, String>>,
    },
    Fork {
        target: SessionRef,
        through: agent_protocol::ids::TurnId,
        now: i64,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Unsubscribe(Uuid),
    Disconnect(u64),
    Shutdown,
}

pub(super) enum Write {
    Setup {
        generation: Uuid,
        update: super::storage::workspace::SetupChange,
        now: i64,
    },
    Discover {
        source: SessionRef,
        header: agent_protocol::conversation::records::Header,
        now: i64,
    },
    Hydrate {
        id: agent_protocol::ids::ItemId,
        version: u64,
        body: agent_protocol::items::ItemBody,
    },
    ImportStatus {
        now: i64,
    },
    ImportFailure {
        operation: Uuid,
        claim: u64,
        error: String,
    },
    Event(
        Event,
        Context<'static>,
        Option<agent_protocol::conversation::records::Header>,
    ),
    Read(agent_protocol::ids::TurnId),
    Workspace {
        cwd: String,
        membership: agent_protocol::models::ProjectMembership,
    },
    Import {
        source: SessionRef,
        operation: Uuid,
        claim: u64,
        response: agent_protocol::models::ThreadResponse,
        now: i64,
    },
}

struct Subscriber {
    session: u64,
    cursor: Cursor,
    window: Window,
    messages: mpsc::Sender<(Vec<u8>, Credit)>,
    bytes: Arc<AtomicUsize>,
    queued_bytes: Arc<AtomicUsize>,
}

// The queued frame owns its charge, including sends already in flight when
// the receiver closes. Dropping a channel message releases both budgets.
struct Credit {
    cost: usize,
    bytes: Arc<AtomicUsize>,
    queued_bytes: Arc<AtomicUsize>,
}
impl Drop for Credit {
    fn drop(&mut self) {
        self.bytes.fetch_sub(self.cost, Relaxed);
        self.queued_bytes.fetch_sub(self.cost, Relaxed);
    }
}

pub(super) struct Read {
    pub cursor: Cursor,
    pub item: Option<agent_protocol::conversation::records::Row>,
}

pub(super) struct Opened {
    pub initial: Message,
    pub subscription: Subscription,
}

pub struct Subscription {
    pub id: Uuid,
    owner: mpsc::UnboundedSender<Input>,
    receiver: mpsc::Receiver<(Vec<u8>, Credit)>,
}
impl Subscription {
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        let (frame, _credit) = self.receiver.recv().await?;
        Some(frame)
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        self.receiver.close();
        let _ = self.owner.send(Input::Unsubscribe(self.id));
    }
}

impl Owners {
    pub(super) async fn write(self: &Arc<Self>, target: &SessionRef, write: Write) -> WriteResult {
        self.enqueue(target, write)?
            .await
            .map_err(|_| "conversation owner stopped before replying".to_owned())?
    }

    /// Queue a write synchronously; dropping its acknowledgement does not cancel it.
    pub(super) fn enqueue(
        self: &Arc<Self>,
        target: &SessionRef,
        write: Write,
    ) -> Result<oneshot::Receiver<WriteResult>, String> {
        let (reply, result) = oneshot::channel();
        self.send(
            target,
            Input::Write {
                write: Box::new(write),
                reply,
            },
        )?;
        Ok(result)
    }

    pub(super) fn new(storage: Arc<Storage>) -> (Arc<Self>, mpsc::UnboundedReceiver<Effect>) {
        let (effects, receiver) = mpsc::unbounded_channel();
        (
            Arc::new(Self {
                storage,
                actors: Mutex::new(HashMap::new()),
                effects,
            }),
            receiver,
        )
    }

    fn send(self: &Arc<Self>, target: &SessionRef, mut input: Input) -> Result<(), String> {
        // Enqueue under the registry lock, so an idle owner cannot be removed
        // between lookup and enqueue while a second owner starts for its key.
        let mut actors = self
            .actors
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        loop {
            if !actors.contains_key(target) {
                let (sender, receiver) = mpsc::unbounded_channel();
                let id = Uuid::new_v4();
                actors.insert(target.clone(), (id, sender.clone()));
                tokio::spawn(run(
                    Arc::downgrade(self),
                    self.storage.clone(),
                    self.effects.clone(),
                    target.clone(),
                    id,
                    sender,
                    receiver,
                ));
            }
            let (_, sender) = actors.get(target).expect("registered owner");
            match sender.send(input) {
                Ok(()) => return Ok(()),
                Err(error) => {
                    input = error.0;
                    actors.remove(target);
                }
            }
        }
    }

    pub(super) async fn subscribe(
        self: &Arc<Self>,
        target: &SessionRef,
        session: u64,
        subscription: Uuid,
        cached: Option<Cursor>,
        window: Option<Window>,
        bytes: Arc<AtomicUsize>,
    ) -> Result<Opened, String> {
        let (reply, result) = oneshot::channel();
        self.send(
            target,
            Input::Subscribe {
                session,
                bytes,
                subscription,
                cached,
                window,
                reply,
            },
        )?;
        result
            .await
            .map_err(|_| "conversation owner stopped before subscribing".to_owned())?
    }

    pub(super) fn disconnect(&self, session: u64) {
        for (_, sender) in self
            .actors
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values()
        {
            let _ = sender.send(Input::Disconnect(session));
        }
    }

    pub(super) async fn fork(
        self: &Arc<Self>,
        source: &SessionRef,
        target: SessionRef,
        through: agent_protocol::ids::TurnId,
        now: i64,
    ) -> Result<(), String> {
        let (reply, result) = oneshot::channel();
        self.send(
            source,
            Input::Fork {
                target,
                through,
                now,
                reply,
            },
        )?;
        result
            .await
            .map_err(|_| "conversation owner stopped during fork".to_owned())?
    }

    pub(super) async fn read(
        self: &Arc<Self>,
        target: &SessionRef,
        session: u64,
        subscription: Uuid,
        item: Option<agent_protocol::ids::ItemId>,
    ) -> Result<Read, String> {
        let (reply, result) = oneshot::channel();
        self.send(
            target,
            Input::Read {
                session,
                subscription,
                item,
                reply,
            },
        )?;
        result
            .await
            .map_err(|_| "conversation owner stopped before reading".to_owned())?
    }
}

async fn run(
    registry: Weak<Owners>,
    storage: Arc<Storage>,
    effects: mpsc::UnboundedSender<Effect>,
    target: SessionRef,
    id: Uuid,
    sender: mpsc::UnboundedSender<Input>,
    mut input: mpsc::UnboundedReceiver<Input>,
) {
    let mut subscribers = HashMap::<Uuid, Subscriber>::new();
    while let Some(message) = input.recv().await {
        match message {
            Input::Write { write, reply } => {
                let records = storage.clone();
                let scope = target.clone();
                let result = tokio::task::spawn_blocking(move || match *write {
                    Write::Setup {
                        generation,
                        update,
                        now,
                    } => records.setup(&scope, generation, update, now),
                    Write::Discover {
                        source,
                        header,
                        now,
                    } => records.discover(&scope, &source, &header, now),
                    Write::Hydrate { id, version, body } => {
                        records.hydrate_body(&scope, &id, version, body)
                    }
                    Write::Event(event, context, create) => {
                        records.decide(&scope, event, context, create.as_ref())
                    }
                    Write::ImportStatus { now } => records.import_status(&scope, now),
                    Write::ImportFailure {
                        operation,
                        claim,
                        error,
                    } => records.import_failed(&scope, operation, claim, error),
                    Write::Read(turn) => records.mark_read(&scope, &turn),
                    Write::Workspace { cwd, membership } => {
                        records.workspace_membership(&scope, &cwd, membership)
                    }
                    Write::Import {
                        source,
                        operation,
                        claim,
                        response,
                        now,
                    } => records.reimport(&scope, &source, operation, claim, &response, now),
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
                let result = async {
                    let committed = result?;
                    publish(&storage, &target, &committed, &mut subscribers).await;
                    for action in committed.step.actions {
                        effects
                            .send(Effect {
                                target: target.clone(),
                                action,
                            })
                            .map_err(|_| "conversation executor stopped".to_owned())?;
                    }
                    Ok::<_, String>((committed.step.outcome, committed.cursor))
                }
                .await;
                if let Err(Err(error)) = reply.send(result) {
                    tracing::warn!(target: "agent", operation="conversation.owner.write", session=%target, %error);
                }
            }
            Input::Subscribe {
                session,
                bytes,
                subscription,
                cached,
                window,
                reply,
            } => {
                let store = storage.clone();
                let thread = target.clone();
                let result =
                    tokio::task::spawn_blocking(move || store.snapshot(&thread, cached, window))
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(|result| result)
                        .and_then(|initial| {
                            let (cursor, floor) = match &initial {
                                Message::Snapshot {
                                    cursor,
                                    window: Some(window),
                                    ..
                                } => (*cursor, *window),
                                Message::Changes { through, .. } => {
                                    (*through, window.ok_or("subscription window is missing")?)
                                }
                                Message::Missing => {
                                    return Err("conversation does not exist".into());
                                }
                                _ => return Err("invalid initial conversation frame".into()),
                            };
                            let id = subscription;
                            if id.is_nil() || subscribers.contains_key(&id) {
                                return Err("subscription ID is missing or already in use".into());
                            }
                            let (messages, receiver) = mpsc::channel(QUEUED_MESSAGES);
                            let queued_bytes = Arc::new(AtomicUsize::new(0));
                            subscribers.insert(
                                id,
                                Subscriber {
                                    session,
                                    cursor,
                                    window: floor,
                                    messages,
                                    bytes,
                                    queued_bytes,
                                },
                            );
                            Ok(Opened {
                                initial,
                                subscription: Subscription {
                                    id,
                                    owner: sender.clone(),
                                    receiver,
                                },
                            })
                        });
                let _ = reply.send(result);
            }
            Input::Fork {
                target: destination,
                through,
                now,
                reply,
            } => {
                let store = storage.clone();
                let source = target.clone();
                let result = tokio::task::spawn_blocking(move || {
                    store.fork(&source, &destination, &through, now)
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
                let _ = reply.send(result);
            }
            Input::Shutdown => break,
            Input::Read {
                session,
                subscription,
                item,
                reply,
            } => {
                let result = async {
                    if let Some(subscriber) = subscribers
                        .get_mut(&subscription)
                        .filter(|subscriber| subscriber.session == session)
                    {
                        let store = storage.clone();
                        let thread = target.clone();
                        let window = subscriber.window;
                        let cached = subscriber.cursor;
                        match tokio::task::spawn_blocking(move || {
                            store.read_page(&thread, cached, window, item.as_ref())
                        })
                        .await
                        {
                            Ok(Ok((mut messages, window, cursor))) => {
                                subscriber.window = window;
                                subscriber.cursor = cursor;
                                let item = messages.iter().find_map(|message| match message {
                                    Message::Hydrate { row, .. } => Some(row.clone()),
                                    _ => None,
                                });
                                for message in &mut messages {
                                    if let Message::Hydrate { row, .. } = message {
                                        agent_protocol::conversation::records::summarize(
                                            std::slice::from_mut(row),
                                            agent_protocol::models::MAX_INLINE_ITEM_BYTES,
                                        )?;
                                    }
                                    subscriber.send(
                                        protocol::encode(message)
                                            .map_err(|error| error.to_string())?,
                                    )?;
                                }
                                Ok(Read { cursor, item })
                            }
                            Ok(Err(error)) => Err(error),
                            Err(error) => Err(error.to_string()),
                        }
                    } else {
                        Err("conversation subscription closed".to_owned())
                    }
                }
                .await;
                if result.is_err()
                    && subscribers
                        .get(&subscription)
                        .is_some_and(|subscriber| subscriber.session == session)
                {
                    subscribers.remove(&subscription);
                }
                let _ = reply.send(result);
            }
            Input::Unsubscribe(id) => {
                subscribers.remove(&id);
            }
            Input::Disconnect(session) => {
                subscribers.retain(|_, subscriber| subscriber.session != session)
            }
        }
        if !subscribers.is_empty() || !input.is_empty() {
            continue;
        }
        let Some(registry) = registry.upgrade() else {
            break;
        };
        let mut actors = registry
            .actors
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !input.is_empty() {
            continue;
        }
        if actors
            .get(&target)
            .is_some_and(|(current, _)| *current == id)
        {
            input.close();
            actors.remove(&target);
        }
        break;
    }
}

async fn publish(
    storage: &Arc<Storage>,
    target: &SessionRef,
    committed: &Committed,
    subscribers: &mut HashMap<Uuid, Subscriber>,
) {
    let reset = if committed.reset {
        let store = storage.clone();
        let thread = target.clone();
        Some(
            tokio::task::spawn_blocking(move || store.snapshot(&thread, None, None))
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result),
        )
    } else {
        None
    };
    let scope = Scope::Conversation(target.clone());
    let mut failed = Vec::new();
    for (id, subscriber) in subscribers.iter_mut() {
        if subscriber.cursor.sequence >= committed.cursor.sequence {
            continue;
        }
        let frame = (|| -> Result<Vec<u8>, String> {
            let message = if committed.reset {
                let snapshot = reset.as_ref().expect("reset frame").clone()?;
                if let Message::Snapshot {
                    cursor,
                    window: Some(window),
                    ..
                } = &snapshot
                {
                    subscriber.window = *window;
                    subscriber.cursor = *cursor;
                }
                snapshot
            } else {
                let mut rows: Vec<_> = committed
                    .rows
                    .iter()
                    .filter(|row| {
                        agent_protocol::sync::contains(
                            &scope,
                            Some(subscriber.window),
                            &row.key,
                            &row.position,
                        )
                    })
                    .cloned()
                    .collect();
                agent_protocol::conversation::records::summarize(
                    &mut rows,
                    agent_protocol::conversation::records::WORK_DETAIL_BYTES,
                )?;
                Message::Changes {
                    from: subscriber.cursor,
                    through: committed.cursor,
                    rows,
                    removed: committed.removed.clone(),
                }
            };
            protocol::encode(message).map_err(|error| error.to_string())
        })();
        let frame = match frame {
            Ok(frame) => frame,
            Err(error) => {
                tracing::error!(target:"agent",operation="conversation.publish",%error);
                failed.push(*id);
                continue;
            }
        };
        if subscriber.send(frame).is_err() {
            failed.push(*id);
            continue;
        }
        if !committed.reset {
            subscriber.cursor = committed.cursor;
        }
    }
    for id in failed {
        subscribers.remove(&id);
    }
}

impl Drop for Owners {
    fn drop(&mut self) {
        for (_, sender) in self
            .actors
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .values()
        {
            let _ = sender.send(Input::Shutdown);
        }
    }
}

#[cfg(test)]
mod tests;

impl Subscriber {
    fn send(&mut self, frame: Vec<u8>) -> Result<(), String> {
        let cost = frame_cost(frame.capacity());
        if self
            .queued_bytes
            .fetch_update(Relaxed, Relaxed, |bytes| {
                bytes
                    .checked_add(cost)
                    .filter(|bytes| *bytes <= STREAM_QUEUED_BYTES)
            })
            .is_err()
        {
            return Err("conversation stream byte budget exceeded".into());
        }
        if self
            .bytes
            .fetch_update(Relaxed, Relaxed, |bytes| {
                bytes
                    .checked_add(cost)
                    .filter(|bytes| *bytes <= MAX_QUEUED_BYTES)
            })
            .is_err()
        {
            self.queued_bytes.fetch_sub(cost, Relaxed);
            return Err("conversation subscription byte budget exceeded".into());
        }
        let credit = Credit {
            cost,
            bytes: self.bytes.clone(),
            queued_bytes: self.queued_bytes.clone(),
        };
        if self.messages.try_send((frame, credit)).is_err() {
            return Err("conversation subscription closed".into());
        }
        Ok(())
    }
}
