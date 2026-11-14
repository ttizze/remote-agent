//! Transport assertions consume the same row contract as native clients.
use agent_protocol::{
    conversation::{Command, InputContent, records},
    error::PeerError,
    ids::{ClientInputId, TurnId},
    models::{ThreadResponse, Turn},
    operations::{self as op},
    protocol::Call,
    session::{ProviderKind, SessionRef},
    sync::Scope,
};
use agent_transport::{client::Client, framing::Reader};
use std::{collections::HashSet, time::Duration};
use uuid::Uuid;

pub struct Conversation {
    reader: Reader,
    subscription: Uuid,
    pub replica: records::Replica,
    completed: HashSet<TurnId>,
}

impl Conversation {
    pub async fn open(client: &Client, session: &SessionRef) -> Result<Self, PeerError> {
        let subscription = Uuid::new_v4();
        let (opened, reader) = client
            .request_stream::<records::Opened>(&Call::SubscribeConversation(records::Subscribe {
                session: session.clone(),
                subscription,
                cached: None,
                window: None,
            }))
            .await?;
        let replica = records::Replica::new(Scope::Conversation(session.clone()), 1)
            .apply(1, opened.initial)
            .expect("valid initial rows");
        let mut conversation = Self {
            reader,
            subscription,
            replica,
            completed: HashSet::new(),
        };
        while conversation
            .view()
            .thread
            .history_read_state
            .as_ref()
            .is_some_and(|state| state.kind == agent_protocol::session::HistoryReadKind::Loading)
        {
            conversation.next().await;
        }
        conversation.completed.extend(
            conversation
                .view()
                .thread
                .turns
                .iter()
                .flatten()
                .filter(|turn| turn.status != agent_protocol::execution::TurnStatus::Running)
                .map(|turn| turn.id.clone()),
        );
        Ok(conversation)
    }

    pub fn view(&self) -> ThreadResponse {
        records::project(&self.replica).unwrap()
    }

    pub async fn next(&mut self) -> ThreadResponse {
        let message: records::Message =
            tokio::time::timeout(Duration::from_secs(15), self.reader.read())
                .await
                .unwrap_or_else(|_| panic!("conversation event deadline: {:?}", self.view()))
                .unwrap()
                .expect("conversation stream closed");
        records::validate_message(&message).unwrap();
        self.replica = self
            .replica
            .clone()
            .apply(1, message)
            .expect("ordered conversation rows");
        self.view()
    }

    pub async fn extend(&mut self, client: &Client) -> ThreadResponse {
        let Scope::Conversation(session) = self.replica.scope() else {
            unreachable!()
        };
        let before = self.replica.window();
        client
            .call(&records::Extend {
                session: session.clone(),
                subscription: self.subscription,
            })
            .await
            .unwrap();
        while self.replica.window() == before {
            self.next().await;
        }
        self.view()
    }

    pub async fn finished(&mut self) -> Turn {
        loop {
            for turn in self.view().thread.turns.iter().flatten() {
                if turn.status != agent_protocol::execution::TurnStatus::Running
                    && self.completed.insert(turn.id.clone())
                {
                    return turn.as_ref().clone();
                }
            }
            self.next().await;
        }
    }

    pub async fn hydrate(
        &mut self,
        client: &Client,
        transport: &agent_transport::transport::Session,
        item: &agent_protocol::ids::ItemId,
    ) -> (agent_protocol::models::Item, usize, Option<u64>) {
        let Scope::Conversation(session) = self.replica.scope() else {
            unreachable!()
        };
        let mut reply = client
            .call(&records::Hydrate {
                session: session.clone(),
                subscription: self.subscription,
                item: item.clone(),
            })
            .await
            .unwrap();
        let wire_bytes = agent_protocol::protocol::encode(&reply).unwrap().len();
        let transfer_size = reply.transfer.as_ref().map(|grant| grant.size);
        let resolved = if let Some(grant) = reply.transfer.take() {
            Some(
                agent_transport::transfers::resolve_item(grant, item, Some(transport))
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        loop {
            let caught_up = self.replica.cursor().is_some_and(|cursor| {
                cursor.store == reply.cursor.store && cursor.sequence >= reply.cursor.sequence
            });
            let ready = self
                .view()
                .thread
                .turns
                .iter()
                .flatten()
                .flat_map(|turn| turn.items.iter().flatten())
                .find(|row| &row.id == item)
                .is_some_and(|item| resolved.is_some() || !item.is_deferred());
            if caught_up && ready {
                break;
            }
            self.next().await;
        }
        if let Some(item) = resolved {
            let row = records::hydrate_body(&self.replica, reply.version.unwrap(), item).unwrap();
            self.replica = self
                .replica
                .apply(
                    1,
                    records::Message::Hydrate {
                        cursor: self.replica.cursor().unwrap(),
                        row,
                    },
                )
                .unwrap();
        }
        let view = self.view();
        let saved = view
            .thread
            .turns
            .iter()
            .flatten()
            .flat_map(|turn| turn.items.iter().flatten())
            .find(|saved| &saved.id == item)
            .unwrap()
            .as_ref()
            .clone();
        (saved, wire_bytes, transfer_size)
    }

    pub async fn input_turn(&mut self, input: &ClientInputId, completed: bool) -> Turn {
        loop {
            if let Some(turn) = self.view().thread.turns.iter().flatten().find(|turn| {
                (!completed || turn.status != agent_protocol::execution::TurnStatus::Running)
                    && turn
                        .items
                        .iter()
                        .flatten()
                        .any(|item| item.client_input_id.as_ref() == Some(input))
            }) {
                return turn.as_ref().clone();
            }
            self.next().await;
        }
    }
}

pub async fn command(
    client: &Client,
    session: &SessionRef,
    operation: Uuid,
    command: Command,
) -> Result<records::Acknowledged, PeerError> {
    let result = client
        .call(&records::Change {
            session: session.clone(),
            operation,
            command,
        })
        .await?;
    result
        .clone()
        .accepted()
        .map_err(|error| PeerError::Remote {
            error: error.message,
            delivery: error.delivery,
            sequence: None,
        })?;
    Ok(result)
}

pub async fn submit(
    client: &Client,
    input: &op::Submission,
) -> Result<records::Acknowledged, PeerError> {
    command(
        client,
        &input.thread_id,
        Uuid::parse_str(input.client_user_message_id.as_str()).unwrap(),
        Command::Submit {
            id: input.client_user_message_id.clone(),
            content: InputContent {
                author: None,
                fresh_context: false,
                parts: input.input.clone(),
                model: input.model.clone(),
                effort: input.effort.clone(),
                service_tier: input.service_tier.clone(),
                follow_up: input.follow_up,
            },
        },
    )
    .await
}

pub fn create_request(
    provider: ProviderKind,
    cwd: Option<String>,
    model: Option<agent_protocol::models::ModelRef>,
) -> records::Create {
    records::Create {
        workspace: None,
        session: SessionRef::new(None, Uuid::new_v4().to_string()).unwrap(),
        provider,
        subscription: Uuid::new_v4(),
        cwd,
        selection: records::Selection {
            model,
            ..Default::default()
        },
        initial: None,
    }
}

pub async fn created(
    client: &Client,
    request: records::Create,
) -> Result<ThreadResponse, PeerError> {
    let opened = client.call(&request).await?;
    let replica = records::Replica::new(Scope::Conversation(opened.session), 1)
        .apply(1, opened.initial)
        .expect("created rows");
    Ok(records::project(&replica).unwrap())
}

pub struct Catalog {
    reader: Reader,
    pub replica: records::Replica,
    pub max_wire_bytes: usize,
}

impl Catalog {
    pub async fn open(client: &Client) -> Self {
        let (opened, reader) = client
            .request_stream::<records::OpenedList>(&Call::SubscribeList(records::SubscribeList {
                refresh: true,
                subscription: Uuid::new_v4(),
                cached: None,
            }))
            .await
            .unwrap();
        records::validate_list(&opened.initial).unwrap();
        let max_wire_bytes = agent_protocol::protocol::encode(&opened.initial)
            .unwrap()
            .len();
        let replica = records::Replica::new(Scope::List, 1)
            .apply(1, opened.initial)
            .unwrap();
        Self {
            reader,
            replica,
            max_wire_bytes,
        }
    }
    pub async fn next(&mut self) {
        let message: records::Message =
            tokio::time::timeout(Duration::from_secs(15), self.reader.read())
                .await
                .expect("catalog deadline")
                .unwrap()
                .expect("catalog closed");
        records::validate_list(&message).unwrap();
        self.max_wire_bytes = self
            .max_wire_bytes
            .max(agent_protocol::protocol::encode(&message).unwrap().len());
        self.replica = self.replica.clone().apply(1, message).unwrap();
    }
    pub async fn wait_rows(&mut self, count: usize) {
        while self
            .replica
            .rows()
            .keys()
            .filter(|key| matches!(key, agent_protocol::sync::Key::Thread(_)))
            .count()
            < count
        {
            self.next().await;
        }
    }
    pub fn page(&self, query: agent_protocol::models::ListQuery) -> agent_core::state::Snapshot {
        use agent_core::state::{Connection, Event, Lists, Snapshot, reduce};
        let subscription = Uuid::new_v4();
        let state = Snapshot {
            connection: Connection {
                connected: true,
                ..Default::default()
            },
            lists: Lists {
                list_query: std::sync::Arc::new(query),
                subscription: Some(subscription),
                replica: Some(std::sync::Arc::new(records::Replica::new(Scope::List, 1))),
                ..Default::default()
            },
            ..Default::default()
        };
        reduce(
            &state,
            "",
            Event::ListRows {
                subscription_id: subscription,
                message: Box::new(self.replica.snapshot().unwrap()),
            },
        )
        .0
    }
    pub fn activity(&self) -> agent_protocol::live_activity::TaskActivityState {
        records::list_rows_activity(self.replica.rows().values())
    }
}

/// The fixture archives provider data under its private source, while public
/// assertions continue to address the original conversation UUID.
pub fn native_source(root: &std::path::Path, id: &SessionRef, provider: ProviderKind) -> String {
    let connection = rusqlite::Connection::open(root.join("bex-conversations.sqlite")).unwrap();
    connection.query_row("SELECT s.native_id FROM sources s JOIN bindings b ON b.source_id=s.id JOIN threads t ON t.key=b.thread WHERE t.provider IS ?1 AND t.id=?2 AND b.provider=?3 AND b.state='ready'",rusqlite::params![id.provider.map(ProviderKind::key),id.id,provider.key()],|row|row.get(0)).unwrap()
}

pub fn list_ids(message: records::Message) -> Vec<SessionRef> {
    let records::Message::Snapshot { rows, .. } = message else {
        panic!("initial list is a snapshot")
    };
    rows.into_iter()
        .filter_map(|row| match row.key {
            agent_protocol::sync::Key::Thread(id) => Some(id),
            _ => None,
        })
        .collect()
}
