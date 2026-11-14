//! Each conversation has one receiver. It commits decisions before publishing
//! rows or asking the resource executor to perform an action.
use super::storage::Storage;
use agent_protocol::{
    conversation::{Action, Context, Event, Outcome},
    session::SessionRef,
    sync::Cursor,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

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
    Fork {
        target: SessionRef,
        through: agent_protocol::ids::TurnId,
        now: i64,
        reply: oneshot::Sender<Result<(), String>>,
    },
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
}

async fn run(
    registry: Weak<Owners>,
    storage: Arc<Storage>,
    effects: mpsc::UnboundedSender<Effect>,
    target: SessionRef,
    id: Uuid,
    mut input: mpsc::UnboundedReceiver<Input>,
) {
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
        }
        if !input.is_empty() {
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
