//! Native binding boundary. Core owns all conversation state and effects.
mod json;
mod snapshot;

use crate::state::Intent;
use crate::transport::{Endpoint, Identity, Relays, Ticket};
use crate::{models::Invitation, state::Snapshot, store::Outcome};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum AgentError {
    #[error("{reason}")]
    Failed { reason: String },
}
fn error(error: impl std::fmt::Display) -> AgentError {
    AgentError::Failed {
        reason: error.to_string(),
    }
}

#[derive(uniffi::Record)]
pub struct Connection {
    pub ticket: String,
    pub identity: Vec<u8>,
    pub invitation: Option<String>,
    pub use_relays: bool,
}

#[uniffi::export]
pub fn generate_identity() -> Vec<u8> {
    Identity::generate().to_bytes().to_vec()
}

#[uniffi::export]
pub fn ticket_identity(ticket: String) -> Result<String, AgentError> {
    Ok(ticket
        .parse::<Ticket>()
        .map_err(error)?
        .node_id()
        .to_string())
}

#[uniffi::export]
pub fn parse_invitation(contents: String, now: u64) -> Result<Invitation, AgentError> {
    let invitation: crate::models::Invitation = serde_json::from_str(&contents).map_err(error)?;
    invitation.endpoint.parse::<Ticket>().map_err(error)?;
    if now >= invitation.expires_at {
        return Err(error("invitation expired"));
    }
    Ok(invitation)
}

#[derive(uniffi::Object)]
pub struct AgentStore {
    store: crate::store::Store,
    endpoint: tokio::sync::Mutex<Option<NativeEndpoint>>,
}
struct NativeEndpoint {
    endpoint: Endpoint,
    use_relays: bool,
}

impl AgentStore {
    async fn connection_endpoint(
        &self,
        connection: Connection,
    ) -> Result<(Endpoint, Ticket, Option<uuid::Uuid>), AgentError> {
        let secret = Zeroizing::new(connection.identity);
        let bytes = Zeroizing::new(
            <[u8; 32]>::try_from(secret.as_slice())
                .map_err(|_| error("identity must contain 32 bytes"))?,
        );
        let identity = Identity::from_bytes(*bytes);
        let ticket = connection.ticket.parse::<Ticket>().map_err(error)?;
        let invitation = connection
            .invitation
            .map(|value| value.parse())
            .transpose()
            .map_err(error)?;
        let mut cached = self.endpoint.lock().await;
        if let Some(cached) = cached.as_ref()
            && cached.endpoint.node_id() == identity.node_id()
            && cached.use_relays == connection.use_relays
        {
            return Ok((cached.endpoint.clone(), ticket, invitation));
        }
        let endpoint = Endpoint::bind(
            identity,
            if connection.use_relays {
                Relays::Default
            } else {
                Relays::Disabled
            },
        )
        .await
        .map_err(error)?;
        *cached = Some(NativeEndpoint {
            endpoint: endpoint.clone(),
            use_relays: connection.use_relays,
        });
        Ok((endpoint, ticket, invitation))
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl AgentStore {
    #[uniffi::constructor]
    pub async fn offline(persisted: Vec<u8>) -> Result<Arc<Self>, AgentError> {
        let snapshot = if persisted.is_empty() {
            crate::state::Snapshot::default()
        } else {
            serde_json::from_slice(&persisted).map_err(error)?
        };
        Ok(Arc::new(Self {
            store: crate::store::Store::offline(snapshot),
            endpoint: Default::default(),
        }))
    }

    #[uniffi::constructor]
    pub async fn connect(
        connection: Connection,
        persisted: Vec<u8>,
    ) -> Result<Arc<Self>, AgentError> {
        let store = Self::offline(persisted).await?;
        store.reconnect(connection).await?;
        Ok(store)
    }

    pub async fn reconnect(&self, connection: Connection) -> Result<(), AgentError> {
        let (endpoint, ticket, invitation) = self.connection_endpoint(connection).await?;
        self.store
            .reconnect(&endpoint, &ticket, invitation)
            .await
            .map_err(error)
    }

    /// Foreground recovery reuses a responsive session and the endpoint identity.
    pub async fn resume(&self, connection: Connection) -> Result<(), AgentError> {
        let (endpoint, ticket, invitation) = self.connection_endpoint(connection).await?;
        if invitation.is_none() {
            self.store.resume(&endpoint, &ticket).await.map_err(error)
        } else {
            self.store
                .reconnect(&endpoint, &ticket, invitation)
                .await
                .map_err(error)
        }
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.store.snapshot()
    }

    /// Subscribe before comparing to avoid losing an update between read and wait.
    pub async fn next_snapshot(
        &self,
        previous: Arc<Snapshot>,
    ) -> Result<Arc<Snapshot>, AgentError> {
        let mut updates = self.store.subscribe();
        loop {
            {
                let current = updates.borrow_and_update();
                if !Arc::ptr_eq(&current, &previous) {
                    return Ok(current.clone());
                }
            }
            updates.changed().await.map_err(error)?;
        }
    }

    /// Enqueue synchronously; native task scheduling cannot reorder UI intents.
    pub fn dispatch(&self, intent: Intent) -> Result<Arc<Receipt>, AgentError> {
        let receipt = self.store.dispatch(intent);
        Ok(Arc::new(Receipt {
            result: Mutex::new(Some(Box::pin(async move { receipt.await.map_err(error) }))),
        }))
    }

    pub async fn shutdown(&self) -> Result<(), AgentError> {
        let result = self.store.close().await.map_err(error);
        if let Some(cached) = self.endpoint.lock().await.take() {
            cached.endpoint.close().await;
        }
        result
    }
}

type Completion = Pin<Box<dyn Future<Output = Result<Outcome, AgentError>> + Send>>;
#[derive(uniffi::Object)]
pub struct Receipt {
    result: Mutex<Option<Completion>>,
}
#[uniffi::export(async_runtime = "tokio")]
impl Receipt {
    pub async fn wait(&self) -> Result<Outcome, AgentError> {
        let result = self
            .result
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| error("receipt was already awaited"))?;
        result.await
    }
}

#[cfg(test)]
mod tests {
    #[allow(dead_code)]
    mod host_fixture {
        include!("../../tests/support/host.rs");
    }
    use super::*;
    use std::time::Duration;
    async fn scoped_incoming(
        host: &Endpoint,
        trust: &crate::transport::Trust,
    ) -> (
        host_fixture::Session,
        host_fixture::Reader,
        host_fixture::Writer,
    ) {
        let session = host
            .accept()
            .await
            .unwrap()
            .unwrap()
            .authorize(trust)
            .unwrap();
        let (session, mut reader, writer) = host_fixture::accept(session).await;
        let request = reader.read_request().await.unwrap().unwrap();
        assert_eq!(request["method"], "host/session/scope");
        writer
            .reply(&request, serde_json::json!({"result":"fixture-storage"}))
            .await
            .unwrap();
        (session, reader, writer)
    }

    #[tokio::test]
    async fn shutdown_releases_a_native_snapshot_waiter() {
        let store = AgentStore::offline(Vec::new()).await.unwrap();
        let previous = store.snapshot();
        let weak = Arc::downgrade(&store);
        let waiting = store.clone();
        let task = tokio::spawn(async move {
            let mut snapshot = previous;
            while let Ok(next) = waiting.next_snapshot(snapshot).await {
                snapshot = next;
            }
        });
        store.shutdown().await.unwrap();
        drop(store);
        tokio::time::timeout(Duration::from_millis(200), task)
            .await
            .expect("shutdown must release a native waiter without task cancellation")
            .unwrap();
        assert!(weak.upgrade().is_none());
    }
    #[tokio::test]
    async fn reconnect_reloads_selected_state_without_native_dispatch() {
        use crate::transport::Trust;
        use serde_json::json;
        for recovery in ["silent", "disconnected", "closed"] {
            tokio::time::timeout(Duration::from_secs(40), async {
            let identity = Identity::generate();
            let trust = Trust { allowed: [identity.node_id()].into(), ..Default::default() };
            let host = Endpoint::bind(Identity::generate(), Relays::Disabled).await.unwrap();
            let connection = || Connection {
                ticket: host.ticket().to_string(), identity: identity.to_bytes().to_vec(),
                invitation: None, use_relays: false,
            };
            let cached = crate::state::Snapshot {
                list_query: Arc::new(crate::models::ListQuery { project_limit: 15, chat_limit: 25, search_term: "retained search".into(), project_thread_limits: [("project".into(), 35)].into() }),
                navigation: Arc::new(crate::state::Navigation { thread_id: Some("thread".into()), draft_key: "thread".into(), ..Default::default() }),
                ..Default::default()
            };
            let mut saved: serde_json::Value = serde_json::from_slice(&cached.serialize_local_state().unwrap()).unwrap();
            assert!(saved.get("list_query").is_none());
            saved["list_query"] = serde_json::to_value(&cached.list_query).unwrap();
            let restored = AgentStore::offline(serde_json::to_vec(&saved).unwrap()).await.unwrap();
            assert_eq!(*restored.snapshot().list_query, crate::models::ListQuery::default());
            restored.shutdown().await.unwrap();
            let store = Arc::new(AgentStore { store: crate::store::Store::offline(cached), endpoint: Default::default() });
            let (connected, (session, reader, writer)) = tokio::join!(store.reconnect(connection()), scoped_incoming(&host, &trust));
            connected.unwrap();
            let first = session;
            let mut old = reader;
            let _first_writer = writer;
            assert!(store.snapshot().connected());
            store.dispatch(Intent::SetDraftText { thread_id: "thread".into(), text: "preserved".into() })
                .unwrap().wait().await.unwrap();
            // Leave every automatic read pending on the old transport.
            let mut methods = std::collections::BTreeSet::new();
            for _ in 0..3 {
                let request = tokio::time::timeout(Duration::from_secs(2), old.read_request()).await
                    .expect("Connected must reload without native intents").unwrap().unwrap();
                methods.insert(request["method"].as_str().unwrap().to_owned());
            }
            assert_eq!(methods, ["host/thread/list", "host/session/open", "model/list"].map(str::to_owned).into());
            let server = async {
                assert!(!matches!(old.read_request().await, Ok(Some(_))),
                    "reconnect must close the old stream without probing it with list/history reads");
                let (next, mut reader, writer) = scoped_incoming(&host, &trust).await;
                let mut requests = std::collections::BTreeMap::new();
                for _ in 0..3 {
                    let request = reader.read_request().await.unwrap().unwrap();
                    assert!(requests.insert(request["method"].as_str().unwrap().to_owned(), request).is_none());
                }
                let list = &requests["host/thread/list"];
                assert_eq!(list["params"]["projectLimit"], 15);
                assert_eq!(list["params"]["chatLimit"], 25);
                assert_eq!(list["params"]["projectThreadLimits"]["project"], 35);
                assert_eq!(list["params"]["searchTerm"], "retained search");
                let open = &requests["host/session/open"];
                assert_eq!(open["params"]["session"]["id"], "thread");
                // Finish the conversation before the lists; no reload invalidates another.
                writer.reply(open, json!({ "result":{"session":{"provider":"codex","id":"thread"},"subscriptionId":uuid::Uuid::new_v4(),"revision":0,"response":{"thread":{"id":"thread","turns":[{"id":"turn","items":[{"id":"answer","type":"agentMessage","text":"after reconnect"}]}]}}}})).await.unwrap();
                writer.reply(&requests["model/list"], json!({ "result":{"data":[{"id":"fresh-model","model":"fresh-model","displayName":"Fresh","defaultReasoningEffort":"medium","supportedReasoningEfforts":[]}],"nextCursor":null}})).await.unwrap();
                writer.reply(list, json!({ "result":{"data":[{"id":"thread","name":"reloaded"}],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false}})).await.unwrap();
                assert!(!matches!(reader.read_request().await, Ok(Some(_))));
                next.close();
            };
            let client = async {
                if recovery == "disconnected" { store.store.disconnect().await.unwrap(); }
                if recovery == "closed" { first.close(); }
                tokio::time::timeout(Duration::from_secs(5), store.reconnect(connection()))
                    .await.expect("reconnect must not wait for the old 30-second RPC deadline").unwrap();
                let mut updates = store.store.subscribe();
                loop {
                    let ready = {
                        let snapshot = updates.borrow_and_update();
                        snapshot.threads.is_some() && !snapshot.models.is_empty() && snapshot.conversations.contains_key("thread")
                    };
                    if ready { break; }
                    updates.changed().await.unwrap();
                }
                let snapshot = store.store.snapshot();
                assert!(snapshot.connected());
                assert!(snapshot.error.is_none());
                assert_eq!(snapshot.navigation.thread_id.as_deref(), Some("thread"));
                assert!(snapshot.threads.as_ref().unwrap().data.iter().any(|thread| thread.id.as_deref() == Some("thread")));
                assert_eq!(snapshot.conversations["thread"].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0].text.as_deref(), Some("after reconnect"));
                assert_eq!(snapshot.models[0].model, "fresh-model");
                assert_eq!(snapshot.drafts["thread"].text, "preserved");
                store.shutdown().await.unwrap();
            };
            tokio::join!(server, client);
            first.close(); host.close().await;
        }).await.unwrap();
        }
    }
    #[tokio::test]
    async fn foreground_reuses_live_session_and_replaces_silent_session() {
        use crate::transport::Trust;
        use serde_json::{Value, json};
        for (selected, mode) in [
            (false, "live"),
            (true, "live"),
            (false, "slow"),
            (true, "slow"),
            (true, "silent"),
            (true, "error"),
        ] {
            let silent = mode == "silent";
            tokio::time::timeout(Duration::from_secs(10), async {
                    let identity = Identity::generate();
                    let trust = Trust { allowed: [identity.node_id()].into(), ..Default::default() };
                    let host = Endpoint::bind(Identity::generate(), Relays::Disabled).await.unwrap();
                    let connection = || Connection {
                        ticket: host.ticket().to_string(), identity: identity.to_bytes().to_vec(),
                        invitation: None, use_relays: false,
                    };
                    let snapshot = Snapshot {
                        navigation: Arc::new(crate::state::Navigation {
                            thread_id: selected.then(|| "thread".into()), draft_key: "thread".into(),
                            ..Default::default()
                        }), ..Default::default()
                    };
                    let store = AgentStore::offline(serde_json::to_vec(&snapshot).unwrap()).await.unwrap();
                    let (connected, (first, mut reader, writer)) = tokio::join!(store.reconnect(connection()), scoped_incoming(&host, &trust));
                    connected.unwrap();
                    let old_ticket = store.endpoint.lock().await.as_ref().unwrap().endpoint.ticket().to_string();
                    let response = |request: &Value, text: &str| {
                        let result = match request["method"].as_str().unwrap() {
                            "host/session/scope" => json!("fixture-storage"),
                            "host/thread/list" => json!({"data":[{"id":"thread","name":text}],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false}),
                            "host/session/open" => json!({"session":{"provider":"codex","id":"thread"},"subscriptionId":uuid::Uuid::new_v4(),"revision":0,"response":{"thread":{"id":"thread","turns":[{"id":"turn","items":[{"id":"answer","type":"agentMessage","text":text}]}]}}}),
                            "model/list" => json!({"data":[],"nextCursor":null}),
                            method => panic!("unexpected request: {method}"),
                        };
                        json!({"result":result})
                    };
                    let server = async {
                        for _ in 0..(2 + usize::from(selected)) {
                            let request = reader.read_request().await.unwrap().unwrap();
                            writer.reply(&request, response(&request, "before")).await.unwrap();
                        }
                        let request = reader.read_request().await.unwrap().unwrap();
                        assert_eq!(request["method"], "host/session/scope");
                        if silent {
                            // Do not answer the Host check. Recovery must replace this
                            // transport without waiting for the normal 30-second deadline.
                            let (next, mut next_reader, next_writer) = scoped_incoming(&host, &trust).await;
                            for _ in 0..(2 + usize::from(selected)) {
                                let request = next_reader.read_request().await.unwrap().unwrap();
                                next_writer.reply(&request, response(&request, "after")).await.unwrap();
                            }
                            assert!(!matches!(reader.read_request().await, Ok(Some(_))));
                            while let Ok(Some(_)) = next_reader.read_request().await {}
                            next.close();
                        } else {
                            let reply = if mode == "error" {
                                json!({"error":{"code":-32000,"message":"history unavailable"}})
                            } else { response(&request, "after") };
                            writer.reply(&request, reply).await.unwrap();
                            if mode == "slow" { tokio::time::sleep(Duration::from_millis(1200)).await; }
                            while let Ok(Some(request)) = reader.read_request().await {
                                let _ = writer.reply(&request, response(&request, "after")).await;
                            }
                        }
                    };
                    let client = async {
                        let mut updates = store.store.subscribe();
                        loop {
                            let ready = { let snapshot = updates.borrow_and_update(); snapshot.threads.is_some() && (!selected || snapshot.conversations.contains_key("thread")) };
                            if ready { break; }
                            updates.changed().await.unwrap();
                        }
                        store.dispatch(Intent::SetDraftText { thread_id: "thread".into(), text: "keep draft".into() }).unwrap().wait().await.unwrap();
                        let resumed = tokio::time::timeout(if silent { Duration::from_secs(2) } else { Duration::from_millis(500) }, store.resume(connection())).await
                            .expect("foreground recovery must not wait for provider reads or shutdown deadlines");
                        if mode == "error" { assert!(resumed.is_err()); } else { resumed.unwrap(); }
                        if mode != "error" { loop {
                            let ready = {
                                let snapshot = updates.borrow_and_update();
                                snapshot.threads.as_ref().is_some_and(|list| list.data.iter().any(|thread| thread.name.as_deref() == Some("after")))
                                    && (!selected || snapshot.conversations["thread"].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0].text.as_deref() == Some("after"))
                            };
                            if ready { break; }
                            updates.changed().await.unwrap();
                        } }
                        assert!(store.snapshot().connected());
                        assert_eq!(store.snapshot().drafts["thread"].text, "keep draft");
                        assert_eq!(store.endpoint.lock().await.as_ref().unwrap().endpoint.ticket().to_string(), old_ticket, "recovery retains the endpoint");
                        store.shutdown().await.unwrap();
                    };
                    tokio::join!(server, client);
                    first.close(); host.close().await;
                }).await.unwrap();
        }
    }

    #[tokio::test]
    async fn foreground_replacement_and_cancellation_respect_connection_ownership() {
        let identity = Identity::generate();
        let trust = crate::transport::Trust {
            allowed: [identity.node_id()].into(),
            ..Default::default()
        };
        let host = Endpoint::bind(Identity::generate(), Relays::Disabled)
            .await
            .unwrap();
        let ticket = host.ticket();
        let first = Endpoint::bind(Identity::from_bytes(identity.to_bytes()), Relays::Disabled)
            .await
            .unwrap();
        let replacement = Endpoint::bind(identity, Relays::Disabled).await.unwrap();
        let (store, (old, old_reader, old_writer)) = tokio::join!(
            crate::store::Store::connect(&first, &ticket, Snapshot::default(), None),
            scoped_incoming(&host, &trust),
        );
        let store = Arc::new(store.unwrap());
        let (resumed, (next, mut reader, _writer)) =
            tokio::time::timeout(Duration::from_millis(500), async {
                tokio::join!(
                    store.resume(&replacement, &ticket),
                    scoped_incoming(&host, &trust)
                )
            })
            .await
            .expect("a changed endpoint must replace the session without probing the old one");
        resumed.unwrap();
        // Leave automatic list/model reads pending, then cancel the foreground read.
        for _ in 0..2 {
            reader.read_request().await.unwrap().unwrap();
        }
        let recovering = store.clone();
        let endpoint = replacement.clone();
        let resume = tokio::spawn(async move { recovering.resume(&endpoint, &ticket).await });
        let request = tokio::time::timeout(Duration::from_secs(1), reader.read_request())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(request["method"], "host/session/scope");
        tokio::time::timeout(Duration::from_millis(500), store.disconnect())
            .await
            .unwrap()
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(500), resume)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert!(!store.snapshot().connected);
        assert!(
            tokio::time::timeout(Duration::from_millis(1100), host.accept())
                .await
                .is_err(),
            "cancelled recovery must not reconnect after its read deadline"
        );
        store.close().await.unwrap();
        drop((old_reader, old_writer));
        old.close();
        next.close();
        first.close().await;
        replacement.close().await;
        host.close().await;
    }

    #[tokio::test]
    async fn active_reads_retain_transport_navigation_and_draft_after_error() {
        use crate::transport::Trust;
        use serde_json::json;
        for selected in [false, true] {
            tokio::time::timeout(Duration::from_secs(40), async {
                let identity = Identity::generate();
                let trust = Trust { allowed: [identity.node_id()].into(), ..Default::default() };
                let host = Endpoint::bind(Identity::generate(), Relays::Disabled).await.unwrap();
                let connection = || Connection { ticket: host.ticket().to_string(), identity: identity.to_bytes().to_vec(), invitation: None, use_relays: false };
                let cached = Snapshot {
                    navigation: Arc::new(crate::state::Navigation { thread_id: selected.then(|| "thread".into()), draft_key: "thread".into(), ..Default::default() }),
                    ..Default::default()
                };
                let store = AgentStore::offline(serde_json::to_vec(&cached).unwrap()).await.unwrap();
                let (connected, (session, mut reader, writer)) = tokio::join!(store.reconnect(connection()), scoped_incoming(&host, &trust));
                connected.unwrap();
                let server = async {
                    for round in 0..(3 + usize::from(selected)) {
                        let mut requests = Vec::new();
                        while requests.len() < (1 + usize::from(selected) + usize::from(round == 0)) {
                            let request = reader.read_request().await.unwrap().expect("refresh must retain the existing stream");
                            requests.push(request);
                        }
                        for request in requests {
                            let result = match request["method"].as_str().unwrap() {
                                "host/thread/list" => json!({"data":[{"id":"thread","name":format!("round {round}")}],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false}),
                                "host/session/open" => json!({"session":{"provider":"codex","id":"thread"},"subscriptionId":uuid::Uuid::new_v4(),"revision":0,"response":{"thread":{"id":"thread","turns":[]}}}),
                                "model/list" if round == 0 => json!({"data":[],"nextCursor":null}),
                                method => panic!("unexpected refresh request {method}"),
                            };
                            if round == 2 && selected && request["method"] == "host/session/open" { continue; }
                            let response = if round == 1 && request["method"] == "host/thread/list" {
                                json!({"error":{"code":-32000,"message":"temporary read error"}})
                            } else { json!({"result":result}) };
                            writer.reply(&request, response).await.unwrap();
                        }
                    }
                    assert!(reader.read_request().await.unwrap().is_none());
                };
                let client = async {
                    let mut updates = store.store.subscribe();
                    loop {
                        let ready = { let snapshot = updates.borrow_and_update(); snapshot.threads.is_some() && (!selected || snapshot.conversations.contains_key("thread")) };
                        if ready { break; }
                        updates.changed().await.unwrap();
                    }
                    store.dispatch(Intent::SetDraftText { thread_id: "thread".into(), text: "preserved".into() }).unwrap().wait().await.unwrap();
                    let refresh = || async {
                        use crate::state::operations as op;
                        let list = store.store.dispatch(Intent::ListThreads(op::ListThreads::new(
                            (*store.snapshot().list_query).clone(),
                        )));
                        let history = async {
                            if selected {
                                store.store.dispatch(Intent::ReadThread(op::ReadThread::new("thread".into()))).await?;
                            }
                            Ok(Outcome::Applied)
                        };
                        let (list, history) = tokio::join!(list, history);
                        list.and(history)
                    };
                    let before = store.snapshot();
                    assert!(refresh().await.is_err());
                    assert!(store.snapshot().connected());
                    assert!(store.snapshot().error.as_ref().unwrap().contains("temporary read error"));
                    if selected {
                        assert!(refresh().await.unwrap_err().to_string().contains("timed out"));
                        assert!(store.snapshot().connected());
                    }
                    refresh().await.unwrap();
                    let after = store.snapshot();
                    assert!(after.connected());
                    assert!(after.error.is_none());
                    assert_eq!(after.epoch, before.epoch);
                    assert_eq!(after.navigation, before.navigation);
                    assert_eq!(after.drafts["thread"].text, "preserved");
                    assert_eq!(after.threads.as_ref().unwrap().data[0].name.as_deref(), Some(if selected { "round 3" } else { "round 2" }));
                    store.shutdown().await.unwrap();
                };
                tokio::join!(server, client);
                session.close();
                host.close().await;
            }).await.unwrap();
        }
    }
}
