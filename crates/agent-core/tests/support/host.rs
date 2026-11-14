#[path = "conversation_rows.rs"]
pub mod rows;
pub use rows::orchestration;
// Scripted unit-test Host over the actual QUIC/MessagePack transport.
// JSON values below are fixture data, never an alternate transport.
use agent_core::state::Snapshot;
use agent_protocol::protocol;
use agent_protocol::{
    models::ThreadResponse,
    protocol::Call,
    session::{SessionChange, SessionRef},
};
use agent_transport::transport;
use agent_transport::{
    client::Client,
    transport::{Endpoint, Identity, Relays, Trust},
};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Mutex, time::Duration};
use tokio::sync::mpsc;

pub struct Request {
    call: Call,
    fixture: Value,
    send: tokio::sync::Mutex<Option<transport::HostPeer>>,
}
impl Request {
    /// Deliberately malformed payloads must reach the real client's typed decoder.
    pub async fn reply_unchecked(&self, result: Value) -> std::io::Result<()> {
        let mut send = self
            .send
            .lock()
            .await
            .take()
            .expect("request already answered");
        agent_transport::framing::write(&mut send, protocol::Response::Success { result }).await
    }
    pub async fn acknowledge(&self) -> std::io::Result<()> {
        let mut send = self
            .send
            .lock()
            .await
            .take()
            .expect("request already answered");
        agent_transport::framing::write(
            &mut send,
            protocol::Response::Success {
                result: agent_protocol::models::Empty {},
            },
        )
        .await
    }
}
impl std::ops::Deref for Request {
    type Target = Value;
    fn deref(&self) -> &Value {
        &self.fixture
    }
}
pub struct Reader {
    incoming: mpsc::Receiver<Request>,
    scripted_composer_catalog: bool,
    scripted_model_catalog: bool,
    scripted_decorations: bool,
}
impl Reader {
    pub fn script_decorations(&mut self) {
        self.scripted_decorations = true;
    }
    pub fn script_model_catalog(&mut self) {
        self.scripted_model_catalog = true;
    }
    pub fn script_composer_catalog(&mut self) {
        self.scripted_composer_catalog = true;
    }
    pub async fn read_request(&mut self) -> std::io::Result<Option<Request>> {
        while let Some(request) = self.incoming.recv().await {
            if !self.scripted_decorations && matches!(&request.call, Call::ReadListDecorations(_)) {
                let mut send = request.send.lock().await.take().unwrap();
                agent_transport::framing::write(
                    &mut send,
                    protocol::Response::Success {
                        result: agent_protocol::operations::ListDecorations {
                            icons: Default::default(),
                            statuses: Vec::new(),
                        },
                    },
                )
                .await?;
                continue;
            }
            if matches!(&request.call, Call::CheckConversationCommands(_)) {
                let mut send = request.send.lock().await.take().unwrap();
                agent_transport::framing::write(
                    &mut send,
                    protocol::Response::Success {
                        result: Vec::<(
                            uuid::Uuid,
                            agent_protocol::conversation::records::Acknowledged,
                        )>::new(),
                    },
                )
                .await?;
                continue;
            }
            if matches!(&request.call, Call::MarkSessionRead(_)) {
                request.acknowledge().await?;
                continue;
            }
            // Unrelated tests configure only Codex. Model concurrency tests
            // explicitly script all providers, just as composer tests do below.
            if !self.scripted_model_catalog
                && matches!(&request.call, Call::ListModels(params) if params.provider != agent_protocol::session::ProviderKind::Codex)
            {
                let mut send = request.send.lock().await.take().unwrap();
                agent_transport::framing::write(
                    &mut send,
                    protocol::Response::Success {
                        result: agent_protocol::operations::ModelPage {
                            data: Vec::new(),
                            next_cursor: None,
                            provider_errors: None,
                        },
                    },
                )
                .await?;
                continue;
            }
            // Unrelated Store tests use an empty catalog. Catalog regressions
            // opt into scripted replies to control the actual RPC boundary.
            if !self.scripted_composer_catalog
                && let Call::ComposerCatalog(params) = &request.call
            {
                let mut send = request.send.lock().await.take().unwrap();
                agent_transport::framing::write(
                    &mut send,
                    protocol::Response::Success {
                        result: agent_protocol::composer::ComposerCatalog {
                            cwd: params.cwd.clone(),
                            ..Default::default()
                        },
                    },
                )
                .await?;
                continue;
            }
            return Ok(Some(request));
        }
        Ok(None)
    }
}
pub struct Session {
    inner: transport::Session,
    blobs: tokio::sync::Mutex<mpsc::Receiver<transport::Stream>>,
}
impl Session {
    pub async fn accept_stream(&self) -> std::io::Result<transport::Stream> {
        self.blobs
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| std::io::Error::other("fixture closed"))
    }
    pub fn close(&self) {
        self.inner.close();
    }
}
#[derive(Default)]
struct State {
    sessions: HashMap<SessionRef, uuid::Uuid>,
    sequence: u64,
    list_subscription: Option<uuid::Uuid>,
    list_replica: Option<agent_protocol::conversation::records::Replica>,
    threads: HashMap<SessionRef, Value>,
    projections: HashMap<SessionRef, agent_protocol::orchestration::projection::ThreadSnapshot>,
}
pub struct Writer {
    state: Mutex<State>,
    events: tokio::sync::Mutex<transport::HostPeer>,
    streams: tokio::sync::Mutex<HashMap<uuid::Uuid, transport::HostPeer>>,
}
pub async fn accept(session: transport::Session) -> (Session, Reader, Writer) {
    let events = session.accept_peer().await.unwrap();
    from_peer(session, events)
}
pub fn from_peer(
    session: transport::Session,
    events: transport::HostPeer,
) -> (Session, Reader, Writer) {
    let (calls, incoming) = mpsc::channel(128);
    let (blobs, incoming_blobs) = mpsc::channel(16);
    let accepting = session.clone();
    tokio::spawn(async move {
        while let Ok(request) = accepting.accept_request().await {
            match request {
                transport::IncomingRequest::Call(request) => {
                    let fixture = json!({"method":request.call.method(),"params":request.call.params_json().unwrap()});
                    if calls
                        .send(Request {
                            call: request.call,
                            fixture,
                            send: tokio::sync::Mutex::new(Some(request.send)),
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                transport::IncomingRequest::Blob(stream) => {
                    if blobs.send(stream).await.is_err() {
                        break;
                    }
                }
                transport::IncomingRequest::Close => break,
            }
        }
    });
    (
        Session {
            inner: session,
            blobs: tokio::sync::Mutex::new(incoming_blobs),
        },
        Reader {
            incoming,
            scripted_composer_catalog: false,
            scripted_model_catalog: false,
            scripted_decorations: false,
        },
        Writer {
            state: Mutex::new(State::default()),
            events: tokio::sync::Mutex::new(events),
            streams: tokio::sync::Mutex::new(HashMap::new()),
        },
    )
}
pub async fn connect(
    snapshot: &Snapshot,
) -> ((Client, agent_transport::framing::Reader), Reader, Writer) {
    let host = Endpoint::bind(Identity::generate(), Relays::Loopback)
        .await
        .unwrap();
    let client = Endpoint::bind(Identity::generate(), Relays::Loopback)
        .await
        .unwrap();
    let trust = Trust {
        allowed: [client.node_id()].into(),
        ..Default::default()
    };
    let ticket = host.ticket();
    let (remote, incoming) = tokio::join!(client.connect(&ticket), host.accept());
    let remote = remote.unwrap();
    let incoming = incoming.unwrap().unwrap().authorize(&trust).unwrap();
    let (peer, (session, reader, writer)) = tokio::join!(
        remote.open_peer(Duration::from_secs(1), 16),
        accept(incoming)
    );
    writer.state.lock().unwrap().threads = snapshot
        .sessions
        .conversations
        .iter()
        .map(|(id, thread)| (id.clone(), json!({"thread":thread})))
        .collect();
    // Keep endpoints and the accepting session alive for the scripted peer.
    tokio::spawn(async move {
        while !reader_closed(&session).await {}
        drop((client, host, remote));
    });
    (peer.unwrap(), reader, writer)
}
async fn reader_closed(session: &Session) -> bool {
    session.blobs.lock().await.recv().await.is_none()
}
impl Writer {
    pub fn detail(
        &self,
        session: &SessionRef,
        item: &agent_protocol::items::Item,
    ) -> agent_protocol::orchestration::TurnItem {
        self.state.lock().unwrap().detail(session, item)
    }
    pub fn projection(
        &self,
        id: &SessionRef,
    ) -> agent_protocol::orchestration::projection::ThreadSnapshot {
        self.state.lock().unwrap().projections[id].clone()
    }
    pub async fn finish_updates(&self) {
        // Only end the latest subscriptions. Closing a replaced stream before
        // the client receives its new open response races with that response.
        let current: Vec<_> = self
            .state
            .lock()
            .unwrap()
            .sessions
            .values()
            .copied()
            .collect();
        let mut streams = self.streams.lock().await;
        for id in current {
            if let Some(mut send) = streams.remove(&id) {
                send.finish().unwrap();
            }
        }
    }
    pub fn current(&self, id: &SessionRef) -> Value {
        self.state
            .lock()
            .unwrap()
            .threads
            .get(id)
            .cloned()
            .unwrap_or_else(
                || json!({"thread":{"id":id,"cwd":"/fixture","status":"idle","turns":[]}}),
            )
    }
    fn prepare(
        &self,
        request: Option<&Request>,
        mut value: Value,
    ) -> std::io::Result<Option<Value>> {
        {
            let mut state = self.state.lock().unwrap();
            if let Some(request) = request {
                if request.call.method() == "host/list/subscribe"
                    && value["result"].get("initial").is_none()
                    && value.get("result").is_some()
                {
                    let page: agent_protocol::models::ThreadList =
                        serde_json::from_value(value["result"].clone())?;
                    value["result"] = json!(agent_protocol::conversation::records::OpenedList {
                        subscription_id: uuid::Uuid::nil(),
                        provider_errors: page.provider_errors.clone().unwrap_or_default(),
                        initial: state.list_snapshot(page)?
                    });
                }
                if request.call.method() == "host/conversation/fork"
                    && value["result"].get("thread").is_some()
                {
                    let target: SessionRef =
                        serde_json::from_value(request["params"]["target"].clone())?;
                    let mut response: ThreadResponse =
                        serde_json::from_value(value["result"].clone())?;
                    response.thread.active_provider = response
                        .thread
                        .active_provider
                        .or_else(|| response.thread.id.as_ref().and_then(|id| id.provider));
                    response.thread.id = Some(target.clone());
                    state.capture(response)?;
                    value["result"] = json!(target);
                }
                if request.call.method() == "host/conversation/change"
                    && value
                        .get("result")
                        .is_some_and(|result| result.get("sequence").is_none())
                {
                    state.sequence = state.sequence.max(7) + 1;
                    value["result"] = json!(agent_protocol::conversation::records::Acknowledged {
                        sequence: state.sequence,
                        accepted_at: "2026-11-14T00:00:00.000Z".into(),
                        error: None,
                    });
                }
                if matches!(
                    request.call.method(),
                    "host/conversation/subscribe" | "host/conversation/create"
                ) {
                    state.opened(request, &mut value)?;
                } else if request.call.method() == "host/conversation/extend"
                    && value.get("result").is_some()
                {
                    state.extended(request, &mut value)?;
                } else if request.call.method() == "host/conversation/hydrate"
                    && value["result"]["item"].get("body").is_some()
                {
                    let target: SessionRef =
                        serde_json::from_value(request["params"]["session"].clone())?;
                    let item: agent_protocol::items::Item =
                        serde_json::from_value(value["result"]["item"].clone())?;
                    let item = state.detail(&target, &item);
                    let transfer: Option<agent_protocol::models::TransferGrant> =
                        serde_json::from_value(value["result"]["transfer"].clone())?;
                    value["result"] = json!(agent_protocol::conversation::records::Hydrated {
                        item: transfer.is_none().then_some(item),
                        transfer
                    });
                }
            }
            if value.get("method").is_some() {
                let method = value["method"].as_str().unwrap();
                let change = if method == "fixture/session/request" {
                    let request: agent_protocol::requests::Request =
                        serde_json::from_value(value["request"].clone())?;
                    Some((
                        serde_json::from_value::<SessionRef>(value["session"].clone()).unwrap(),
                        SessionChange::Request { request },
                    ))
                } else {
                    if method == "fixture/session/change" {
                        Some((
                            serde_json::from_value::<SessionRef>(value["session"].clone())?,
                            serde_json::from_value::<SessionChange>(value["change"].clone())?,
                        ))
                    } else {
                        None
                    }
                };
                if let Some((id, change)) = change {
                    if let Some(response) = state.threads.get_mut(&id) {
                        let mut decoded: ThreadResponse = serde_json::from_value(response.clone())?;
                        decoded.thread = change
                            .apply(&decoded.thread)
                            .map_err(std::io::Error::other)?;
                        *response = json!(decoded);
                    }
                    if !state.sessions.contains_key(&id) && state.list_subscription.is_none() {
                        return Ok(None);
                    }
                    value = json!({"fixtureChanged":id});
                }
            }
        }
        Ok(Some(value))
    }
    pub async fn send_stream(
        &self,
        subscription: uuid::Uuid,
        message: impl serde::Serialize,
    ) -> std::io::Result<()> {
        let latest = {
            let state = self.state.lock().unwrap();
            state.list_subscription == Some(subscription)
                || state.sessions.values().any(|id| *id == subscription)
        };
        let mut streams = self.streams.lock().await;
        let Some(stream) = streams.get_mut(&subscription) else {
            return if latest {
                Err(std::io::Error::other("subscription is missing"))
            } else {
                Ok(())
            };
        };
        let result = agent_transport::framing::write(stream, message).await;
        if result.is_err() && !latest {
            streams.remove(&subscription);
            Ok(())
        } else {
            result
        }
    }
    pub async fn reply(&self, request: &Request, value: Value) -> std::io::Result<()> {
        let mut value = self.prepare(Some(request), value)?.unwrap();
        // The corpus is data for a typed result; only its MessagePack frame is sent.
        value["id"] = 0.into();
        let row_subscription = if request.call.method() == "host/list/subscribe"
            && value.get("result").is_some()
        {
            let subscription: uuid::Uuid =
                serde_json::from_value(request["params"]["subscription"].clone())?;
            let opened: agent_protocol::conversation::records::OpenedList =
                serde_json::from_value(value["result"].clone())?;
            let mut state = self.state.lock().unwrap();
            let replica = agent_protocol::conversation::records::Replica::new(
                agent_protocol::sync::Scope::List,
                1,
            )
            .apply(1, opened.initial)
            .map_err(std::io::Error::other)?;
            state.sequence = state.sequence.max(replica.cursor().unwrap().sequence);
            state.list_replica = Some(replica);
            state.list_subscription = Some(subscription);
            Some(subscription)
        } else if matches!(
            request.call.method(),
            "host/conversation/subscribe" | "host/conversation/create"
        ) && value.get("result").is_some()
        {
            let target: SessionRef = serde_json::from_value(request["params"]["session"].clone())?;
            let subscription: uuid::Uuid =
                serde_json::from_value(request["params"]["subscription"].clone())?;
            let mut state = self.state.lock().unwrap();
            state.sessions.insert(target.clone(), subscription);
            if let Ok(opened) = serde_json::from_value::<
                agent_protocol::conversation::records::Opened,
            >(value["result"].clone())
            {
                for item in opened.initial {
                    if let agent_protocol::orchestration::projection::ThreadStreamItem::Snapshot(
                        snapshot,
                    ) = item
                    {
                        state.sequence = state.sequence.max(snapshot.snapshot_sequence);
                    }
                }
            }
            Some(subscription)
        } else {
            None
        };
        let response = protocol::json_boundary::response(request.call.method(), &value.to_string())
            .map_err(std::io::Error::other)?;
        let mut send = request
            .send
            .lock()
            .await
            .take()
            .expect("request already answered");
        agent_transport::framing::write(&mut send, response).await?;
        if let Some(id) = row_subscription {
            self.streams.lock().await.insert(id, send);
        }
        if let Call::CreateConversation(request) = &request.call {
            let change = self.state.lock().unwrap().list_change(&request.session)?;
            if let Some((subscription, message)) = change {
                self.send_stream(subscription, message).await?;
            }
        }
        Ok(())
    }
    pub async fn notify(&self, value: Value) -> std::io::Result<()> {
        let prepared = self.prepare(None, value)?;
        let Some(value) = prepared else {
            return Ok(());
        };
        let messages = {
            let mut state = self.state.lock().unwrap();
            let id = serde_json::from_value::<SessionRef>(value["fixtureChanged"].clone()).ok();
            if let Some(id) = id {
                state.sequence += 1;
                let Some(thread) = state.threads.get(&id) else {
                    return Ok(());
                };
                let response: ThreadResponse = serde_json::from_value(thread.clone())?;
                let snapshot = state.capture(response)?;
                Some((
                    state.sessions.get(&id).copied(),
                    snapshot,
                    state.list_change(&id)?,
                ))
            } else {
                None
            }
        };
        if let Some((subscription, snapshot, list)) = messages {
            if let Some(subscription) = subscription {
                self.send_stream(
                    subscription,
                    agent_protocol::orchestration::projection::ThreadStreamItem::Snapshot(snapshot),
                )
                .await?;
            }
            if let Some((subscription, message)) = list {
                self.send_stream(subscription, message).await?;
            }
            return Ok(());
        }

        let notification = protocol::json_boundary::notification(
            value["method"].as_str().unwrap(),
            value["params"].clone(),
        )
        .map_err(std::io::Error::other)?;
        agent_transport::framing::write(&mut *self.events.lock().await, notification).await?;
        Ok(())
    }
}

impl State {
    fn list_snapshot(
        &mut self,
        page: agent_protocol::models::ThreadList,
    ) -> std::io::Result<agent_protocol::conversation::records::Message> {
        use agent_protocol::sync::Cursor;
        self.sequence = self.sequence.max(7) + 1;
        for thread in &page.data {
            if let Some(id) = &thread.id {
                self.threads.insert(
                    id.clone(),
                    serde_json::to_value(ThreadResponse {
                        thread: thread.clone(),
                        ..Default::default()
                    })?,
                );
            }
        }
        let message = rows::catalog(
            page,
            Cursor {
                store: uuid::Uuid::from_u128(1),
                sequence: self.sequence,
            },
        );
        Ok(message)
    }

    fn list_change(
        &mut self,
        id: &SessionRef,
    ) -> std::io::Result<Option<(uuid::Uuid, agent_protocol::conversation::records::Message)>> {
        use agent_protocol::sync::{Cursor, Key, Message};
        let (Some(subscription), Some(replica)) =
            (self.list_subscription, self.list_replica.as_ref())
        else {
            return Ok(None);
        };
        let Some(thread) = self.threads.get(id) else {
            return Ok(None);
        };
        let response: ThreadResponse = serde_json::from_value(thread.clone())?;
        let Message::Snapshot { rows, .. } = rows::catalog(
            agent_protocol::models::ThreadList {
                data: vec![response.thread],
                ..Default::default()
            },
            Cursor {
                store: uuid::Uuid::from_u128(1),
                sequence: self.sequence,
            },
        ) else {
            unreachable!()
        };
        let Some(header) = rows
            .into_iter()
            .find(|row| row.key == Key::Thread(id.clone()))
        else {
            return Ok(None);
        };
        let from = replica.cursor().unwrap();
        if header.version <= from.sequence {
            return Ok(None);
        }
        let message = Message::Events {
            through: Cursor {
                sequence: self.sequence,
                ..from
            },
            events: vec![agent_protocol::sync::Event {
                sequence: (Cursor {
                    sequence: self.sequence,
                    ..from
                })
                .sequence,
                rows: vec![header],
                removed: Vec::new(),
            }],
        };
        self.list_replica = Some(
            replica
                .apply(1, message.clone())
                .map_err(std::io::Error::other)?,
        );
        Ok(Some((subscription, message)))
    }

    fn opened(&mut self, request: &Request, value: &mut Value) -> std::io::Result<()> {
        if let Some(response) = value
            .get("result")
            .filter(|value| value.get("thread").is_some())
            .cloned()
        {
            let mut response: ThreadResponse = serde_json::from_value(response)?;
            if request.call.method() == "host/conversation/create" {
                response.thread.active_provider = Some(serde_json::from_value(
                    request["params"]["provider"].clone(),
                )?);
                response.thread.id = Some(serde_json::from_value(
                    request["params"]["session"].clone(),
                )?);
            }
            let Some(target) = response.thread.id.clone() else {
                value["result"] =
                    json!({"session":request["params"]["session"],"initial":{"Missing":null}});
                return Ok(());
            };
            self.sequence = self.sequence.max(7);
            if self.list_replica.is_some() {
                self.sequence += 1;
            }
            if matches!(&request.call, Call::SubscribeConversation(params) if params.after_sequence.is_some())
            {
                self.sequence += 1;
            }
            let provider = response
                .thread
                .active_provider
                .or(response.thread.id.as_ref().and_then(|id| id.provider))
                .unwrap();
            let capabilities = response.thread.capabilities.unwrap_or_default();
            let snapshot = self.capture(response)?;
            value["result"] = json!(agent_protocol::conversation::records::Opened {
                session: target,
                subscription_id: uuid::Uuid::nil(),
                initial: vec![
                    agent_protocol::orchestration::projection::ThreadStreamItem::Snapshot(snapshot),
                    agent_protocol::orchestration::projection::ThreadStreamItem::Synchronized
                ],
                provider,
                capabilities,
                project_root: None,
            });
        } else if let Ok(opened) = serde_json::from_value::<
            agent_protocol::conversation::records::Opened,
        >(value["result"].clone())
        {
            let mut snapshot = None;
            for item in opened.initial {
                snapshot = agent_core::orchestration::receive(
                    &opened.session.application_id(),
                    snapshot.as_ref(),
                    item,
                )
                .map_err(std::io::Error::other)?;
            }
            if let Some(snapshot) = snapshot {
                let response = agent_core::orchestration::project(
                    &snapshot,
                    &opened.session,
                    opened.provider,
                    opened.capabilities,
                    opened.project_root.as_deref(),
                    &Default::default(),
                )
                .map_err(std::io::Error::other)?;
                self.threads
                    .insert(opened.session.clone(), serde_json::to_value(response)?);
                self.sequence = self.sequence.max(snapshot.snapshot_sequence);
                self.projections.insert(opened.session, snapshot);
            }
        }
        Ok(())
    }
}

impl State {
    fn detail(
        &self,
        session: &SessionRef,
        native: &agent_protocol::items::Item,
    ) -> agent_protocol::orchestration::TurnItem {
        let (identity, snapshot) = self
            .projections
            .iter()
            .find(|(identity, _)| identity.application_id() == session.application_id())
            .unwrap();
        let original = snapshot
            .projection
            .turn_items
            .iter()
            .find(|item| item.id == native.id.as_str())
            .expect("hydrated item exists");
        let response: ThreadResponse =
            serde_json::from_value(self.threads[identity].clone()).unwrap();
        let provider = response
            .thread
            .active_provider
            .or(identity.provider)
            .unwrap();
        let mut item = orchestration::item_record(
            &original.thread_id,
            original.run_id.as_deref(),
            native,
            provider,
            false,
        );
        item.id = original.id.clone();
        item.ordinal = original.ordinal;
        item.status = original.status;
        item.updated_at = original.updated_at.clone();
        item.started_at = original.started_at.clone();
        item.completed_at = original.completed_at.clone();
        item
    }

    fn capture(
        &mut self,
        response: ThreadResponse,
    ) -> std::io::Result<agent_protocol::orchestration::projection::ThreadSnapshot> {
        let id = response.thread.id.clone().unwrap();
        let mut snapshot = orchestration::snapshot(response.clone(), self.sequence.max(7));
        if snapshot.has_more_history && snapshot.history_cursor.is_none() {
            snapshot.history_cursor =
                Some(format!("fixture-history:{}", snapshot.snapshot_sequence));
        }
        // Preserve positions while native fixture pages expand backwards.
        let previous = self.projections.get(&id);
        let offset = snapshot
            .projection
            .visible_turn_items
            .iter()
            .enumerate()
            .find_map(|(index, incoming)| {
                previous?
                    .projection
                    .visible_turn_items
                    .iter()
                    .find(|saved| saved.source_item_id == incoming.source_item_id)
                    .and_then(|saved| saved.position.checked_sub(index as u64 + 1))
            })
            .unwrap_or(if snapshot.has_more_history { 1_000 } else { 0 });
        for item in &mut snapshot.projection.visible_turn_items {
            item.position += offset;
        }
        self.threads
            .insert(id.clone(), serde_json::to_value(response)?);
        self.projections.insert(id, snapshot.clone());
        Ok(snapshot)
    }

    fn extended(&mut self, request: &Request, value: &mut Value) -> std::io::Result<()> {
        if value["result"].get("snapshotSequence").is_some() {
            return Ok(());
        }
        let id: SessionRef = serde_json::from_value(request["params"]["session"].clone())?;
        let mut response: ThreadResponse = serde_json::from_value(self.threads[&id].clone())?;
        let (turns, next) = if value["result"].get("thread").is_some() {
            let page: ThreadResponse = serde_json::from_value(value["result"].clone())?;
            let next = page.thread.history_cursor.or_else(|| {
                (page.thread.history_has_more == Some(true)).then(|| "fixture-older".into())
            });
            (page.thread.turns.unwrap_or_default(), next)
        } else {
            let page: agent_protocol::session::HistoryPage =
                serde_json::from_value(value["result"].clone())?;
            (page.turns, page.next_cursor)
        };
        let incoming_ids: std::collections::HashSet<_> =
            turns.iter().map(|turn| turn.id.clone()).collect();
        let mut joined = turns;
        joined.extend(
            response
                .thread
                .turns
                .take()
                .unwrap_or_default()
                .into_iter()
                .filter(|turn| !incoming_ids.contains(&turn.id)),
        );
        response.thread.turns = Some(joined);
        response.thread.history_cursor = next.clone();
        response.thread.history_has_more = Some(next.is_some());
        let snapshot = self.capture(response)?;
        value["result"] = json!(
            agent_protocol::orchestration::projection::ThreadHistoryPage {
                snapshot_sequence: snapshot.snapshot_sequence,
                items: snapshot
                    .projection
                    .visible_turn_items
                    .into_iter()
                    .filter(|item| item
                        .item
                        .run_id
                        .as_ref()
                        .is_some_and(|run| incoming_ids.iter().any(|id| id.as_str() == run)))
                    .collect(),
                next_cursor: next.clone(),
                has_more_history: next.is_some(),
            }
        );
        Ok(())
    }
}
