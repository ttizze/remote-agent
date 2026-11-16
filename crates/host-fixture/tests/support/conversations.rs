//! Transport assertions consume the same canonical stream as native clients.
use agent_protocol::orchestration::{
    self as model,
    projection::{ThreadSnapshot, ThreadStreamItem},
};
use agent_protocol::{
    conversation::{Command, InputContent, records},
    error::PeerError,
    ids::{ClientInputId, TurnId},
    models::{ThreadResponse, Turn},
    operations::{self as op},
    protocol::Call,
    session::{ProviderKind, SessionRef},
};
use agent_transport::{client::Client, framing::Reader};
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
    time::Duration,
};
use uuid::Uuid;

pub struct Projection {
    pub snapshot: ThreadSnapshot,
    session: SessionRef,
    provider: ProviderKind,
    capabilities: agent_protocol::session::Capabilities,
    project_root: Option<String>,
    details: BTreeMap<(String, String), Arc<model::TurnItem>>,
}

impl Projection {
    pub fn new(opened: records::Opened) -> Self {
        let mut snapshot = None;
        for message in opened.initial {
            snapshot = agent_core::orchestration::receive(
                &opened.session.application_id(),
                snapshot.as_ref(),
                message,
            )
            .unwrap();
        }
        Self {
            snapshot: snapshot.expect("opening includes a canonical snapshot"),
            session: opened.session,
            provider: opened.provider,
            capabilities: records::capabilities(opened.capabilities),
            project_root: opened.project_root,
            details: BTreeMap::new(),
        }
    }

    pub fn receive(&mut self, message: ThreadStreamItem) {
        self.snapshot = agent_core::orchestration::receive(
            &self.session.application_id(),
            Some(&self.snapshot),
            message,
        )
        .unwrap()
        .expect("snapshot remains available");
    }

    pub fn view(&self) -> ThreadResponse {
        agent_core::orchestration::project(
            &self.snapshot,
            &self.session,
            self.provider,
            self.capabilities,
            self.project_root.as_deref(),
            &self.details,
        )
        .unwrap()
    }
}

pub struct Conversation {
    reader: Reader,
    pub projection: Projection,
    completed: HashSet<TurnId>,
}

impl Conversation {
    pub async fn open(client: &Client, session: &SessionRef) -> Result<Self, PeerError> {
        let (opened, reader) = client
            .request_stream::<records::Opened>(&Call::SubscribeConversation(records::Subscribe {
                session: session.clone(),
                subscription: Uuid::new_v4(),
                after_sequence: None,
            }))
            .await?;
        let mut conversation = Self {
            reader,
            projection: Projection::new(opened),
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
        self.projection.view()
    }

    pub async fn next(&mut self) -> ThreadResponse {
        let message: ThreadStreamItem =
            tokio::time::timeout(Duration::from_secs(15), self.reader.read())
                .await
                .unwrap_or_else(|_| panic!("conversation event deadline: {:?}", self.view()))
                .unwrap()
                .expect("conversation stream closed");
        self.projection.receive(message);
        self.view()
    }

    pub async fn extend(&mut self, client: &Client) -> ThreadResponse {
        let cursor = self
            .projection
            .snapshot
            .history_cursor
            .clone()
            .expect("older history cursor");
        let page = client
            .call(&records::Extend {
                session: self.projection.session.clone(),
                cursor: cursor.clone(),
            })
            .await
            .unwrap();
        self.projection.snapshot =
            agent_core::orchestration::receive_history(&self.projection.snapshot, &cursor, page)
                .expect("requested history page is current");
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
        let reply = client
            .call(&records::Hydrate {
                session: self.projection.session.clone(),
                item: item.clone(),
            })
            .await
            .unwrap();
        let wire_bytes = agent_protocol::protocol::encode(&reply).unwrap().len();
        let transfer_size = reply.transfer.as_ref().map(|grant| grant.size);
        let detail = if let Some(grant) = reply.transfer {
            agent_transport::transfers::resolve_item(grant, item, Some(transport))
                .await
                .unwrap()
        } else {
            reply.item.expect("requested body exists")
        };
        assert_eq!(detail.id, item.as_str());
        self.projection.details.insert(
            (detail.thread_id.clone(), detail.id.clone()),
            Arc::new(detail),
        );
        loop {
            if let Some(saved) = self
                .view()
                .thread
                .turns
                .iter()
                .flatten()
                .flat_map(|turn| turn.items.iter().flatten())
                .find(|saved| &saved.id == item && !saved.is_deferred())
            {
                return (saved.as_ref().clone(), wire_bytes, transfer_size);
            }
            self.next().await;
        }
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
    Ok(Projection::new(opened).view())
}

pub struct Catalog {
    reader: Reader,
    pub snapshot: agent_protocol::orchestration::shell::ShellSnapshot,
    pub max_wire_bytes: usize,
}

impl Catalog {
    pub async fn open(client: &Client) -> Self {
        let (opened, reader) = client
            .request_stream::<records::OpenedList>(&Call::SubscribeList(records::SubscribeList {
                refresh: true,
                subscription: Uuid::new_v4(),
                after_sequence: None,
            }))
            .await
            .unwrap();
        records::validate_list(&opened.initial).unwrap();
        let max_wire_bytes = agent_protocol::protocol::encode(&opened.initial)
            .unwrap()
            .len();
        let mut snapshot = None;
        for item in opened.initial {
            snapshot =
                agent_core::orchestration::shell::receive(snapshot.as_ref(), item, true).unwrap();
        }
        Self {
            reader,
            snapshot: snapshot.unwrap(),
            max_wire_bytes,
        }
    }
    pub async fn next(&mut self) {
        let message = tokio::time::timeout(
            Duration::from_secs(15),
            self.reader
                .read::<agent_protocol::orchestration::shell::ShellStreamItem>(),
        )
        .await
        .expect("catalog deadline")
        .unwrap()
        .expect("catalog closed");
        self.max_wire_bytes = self
            .max_wire_bytes
            .max(agent_protocol::protocol::encode(&message).unwrap().len());
        self.snapshot =
            agent_core::orchestration::shell::receive(Some(&self.snapshot), message, false)
                .unwrap()
                .unwrap();
    }
    pub async fn wait_rows(&mut self, count: usize) {
        while self.snapshot.threads.len() < count {
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
                ..Default::default()
            },
            ..Default::default()
        };
        reduce(
            &state,
            "",
            Event::ListStream {
                subscription_id: subscription,
                message: Box::new(
                    agent_protocol::orchestration::shell::ShellStreamItem::Snapshot {
                        snapshot: self.snapshot.clone(),
                        resolved_repository_identity_roots: None,
                    },
                ),
            },
        )
        .0
    }
    pub fn activity(&self) -> agent_protocol::live_activity::TaskActivityState {
        agent_protocol::orchestration::shell::activity(
            self.snapshot.threads.iter().map(AsRef::as_ref),
        )
        .unwrap()
    }
}

/// The fixture archives provider data under its private source, while public
/// assertions continue to address the original conversation UUID.
pub fn native_source(root: &std::path::Path, id: &SessionRef, provider: ProviderKind) -> String {
    let source = host_daemon::canonical_test_support::active_native_source(
        &root.join(host_daemon::CONVERSATION_DATABASE_FILE),
        id,
    )
    .unwrap()
    .expect("canonical active native source");
    assert_eq!(
        source.provider,
        Some(provider),
        "fixture expects the selected provider"
    );
    source.id
}

pub fn list_ids(
    items: Vec<agent_protocol::orchestration::shell::ShellStreamItem>,
) -> Vec<SessionRef> {
    let mut snapshot = None;
    for item in items {
        snapshot =
            agent_core::orchestration::shell::receive(snapshot.as_ref(), item, true).unwrap();
    }
    snapshot
        .unwrap()
        .threads
        .iter()
        .map(|thread| {
            agent_protocol::orchestration::shell::public_session_ref(&thread.thread.id).unwrap()
        })
        .collect()
}
