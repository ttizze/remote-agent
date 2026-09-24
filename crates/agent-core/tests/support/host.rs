// Scripted unit-test Host over the actual QUIC/Postcard transport.
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
#[path = "provider_fixture.rs"]
mod provider_fixture;

pub struct Request {
    call: Call,
    fixture: Value,
    send: tokio::sync::Mutex<Option<transport::HostPeer>>,
}
impl std::ops::Deref for Request {
    type Target = Value;
    fn deref(&self) -> &Value {
        &self.fixture
    }
}
pub struct Reader {
    incoming: mpsc::Receiver<Request>,
}
impl Reader {
    pub async fn read_request(&mut self) -> std::io::Result<Option<Request>> {
        Ok(self.incoming.recv().await)
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
    sessions: HashMap<String, uuid::Uuid>,
    threads: HashMap<String, Value>,
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
        Reader { incoming },
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
    let host = Endpoint::bind(Identity::generate(), Relays::Disabled)
        .await
        .unwrap();
    let client = Endpoint::bind(Identity::generate(), Relays::Disabled)
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
    pub fn current(&self, id: &str) -> Value {
        self.state
            .lock()
            .unwrap()
            .threads
            .get(id)
            .cloned()
            .unwrap_or_else(
                || json!({"thread":{"id":id,"cwd":"/fixture","status":{"type":"idle"},"turns":[]}}),
            )
    }
    fn prepare(
        &self,
        request: Option<&Request>,
        mut value: Value,
    ) -> std::io::Result<Option<Value>> {
        {
            let mut state = self.state.lock().unwrap();
            if let Some(response) = value
                .get("result")
                .filter(|result| result.get("thread").is_some())
                .cloned()
            {
                if let Some(id) = response["thread"]["id"].as_str() {
                    state.threads.insert(id.into(), response.clone());
                }
                if request.is_some_and(|request| request["method"] == "host/session/open") {
                    let target: SessionRef =
                        serde_json::from_value(request.unwrap()["params"]["session"].clone())?;
                    let subscription = uuid::Uuid::new_v4();
                    state.sessions.insert(target.thread_id(), subscription);
                    let mut response = response;
                    response["thread"]["capabilities"] = json!({"additionalInput":true,"fork":true,"rename":true,"modelChange":true});
                    value["result"] =
                        json!({"session":target,"subscriptionId":subscription,"response":response});
                }
            } else if value.get("method").is_some() {
                let method = value["method"].as_str().unwrap();
                let change = if value.get("id").is_some() {
                    let request: agent_protocol::operations::ServerRequest =
                        serde_json::from_value(value.clone())?;
                    Some((
                        value["params"]["threadId"].as_str().unwrap().to_owned(),
                        SessionChange::Request { request },
                    ))
                } else {
                    provider_fixture::notification_change(method, value["params"].clone())?
                };
                if let Some((id, change)) = change {
                    if let Some(response) = state.threads.get_mut(&id) {
                        let mut decoded: ThreadResponse = serde_json::from_value(response.clone())?;
                        if let Ok(thread) = change.apply(&decoded.thread) {
                            decoded.thread = thread;
                            *response = json!(decoded);
                        }
                    }
                    let Some(subscription) = state.sessions.get_mut(&id) else {
                        return Ok(None);
                    };
                    value = json!({"method":"host/session/update","params":{"subscriptionId":subscription,"change":change}});
                }
            }
        }
        Ok(Some(value))
    }
    pub async fn reply(&self, request: &Request, value: Value) -> std::io::Result<()> {
        let mut value = self.prepare(Some(request), value)?.unwrap();
        // The corpus is data for a typed result; only its Postcard frame is sent.
        value["id"] = 0.into();
        let response = protocol::json_boundary::response(request.call.method(), &value.to_string())
            .map_err(std::io::Error::other)?;
        let mut send = request
            .send
            .lock()
            .await
            .take()
            .expect("request already answered");
        agent_transport::framing::write(&mut send, response).await?;
        if request.call.method() == "host/session/open"
            && let Some(id) = value["result"]["subscriptionId"].as_str()
        {
            self.streams.lock().await.insert(id.parse().unwrap(), send);
        }
        Ok(())
    }
    pub async fn notify(&self, value: Value) -> std::io::Result<()> {
        let Some(value) = self.prepare(None, value)? else {
            return Ok(());
        };
        if value["method"] == "host/session/update" {
            let update: agent_protocol::session::SessionUpdate =
                serde_json::from_value(value["params"].clone())?;
            if let Some(send) = self.streams.lock().await.get_mut(&update.subscription_id) {
                agent_transport::framing::write(send, update.change).await?;
            }
        } else {
            let notification = protocol::json_boundary::notification(
                value["method"].as_str().unwrap(),
                value["params"].clone(),
            )
            .map_err(std::io::Error::other)?;
            agent_transport::framing::write(&mut *self.events.lock().await, notification).await?;
        }
        Ok(())
    }
}
