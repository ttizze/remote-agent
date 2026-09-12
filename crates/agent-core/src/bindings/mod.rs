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
        let secret = Zeroizing::new(connection.identity);
        let snapshot = self.store.snapshot();
        if snapshot.connected() {
            use crate::{peer::PeerError, state::operations as op};
            let list = self
                .store
                .dispatch(Intent::ListThreads(op::ListThreads::new(
                    (*snapshot.list_query).clone(),
                )));
            let history = async {
                if let Some(id) = &snapshot.navigation.thread_id {
                    self.store
                        .dispatch(Intent::ReadThread(op::ReadThread::new(id.clone())))
                        .await?;
                }
                Ok(Outcome::Applied)
            };
            let (list, history) = tokio::join!(list, history);
            let errors = [list.err(), history.err()];
            if !errors
                .iter()
                .flatten()
                .any(|error| matches!(error, PeerError::ConnectionClosed(_)))
            {
                // Navigation can supersede an in-flight refresh and its errors.
                return if self.store.snapshot().epoch != snapshot.epoch {
                    Ok(())
                } else {
                    errors
                        .into_iter()
                        .flatten()
                        .next()
                        .map_or(Ok(()), |failure| Err(error(failure)))
                };
            }
        }
        self.store.disconnect().await.map_err(error)?;
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
        self.store
            .reconnect(&endpoint, &ticket, invitation)
            .await
            .map_err(error)
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
        self.store.close().await.map_err(error)
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
    use super::*;
    use std::time::Duration;

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
        use crate::peer::{JsonlReader, JsonlWriter};
        use crate::transport::Trust;
        use serde_json::{Value, json};
        for recovery in ["disconnected", "closed", "silent"] {
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
            let store = AgentStore::offline(serde_json::to_vec(&cached).unwrap()).await.unwrap();
            let (connected, incoming) = tokio::join!(store.reconnect(connection()), host.accept());
            connected.unwrap();
            let first = incoming.unwrap().unwrap().authorize(&trust).unwrap();
            let stream = first.accept_stream().await.unwrap();
            let (read, _write) = tokio::io::split(stream);
            let mut old = JsonlReader::new(read);
            assert_eq!(old.read_line().await.unwrap().as_deref(), Some(""));
            assert!(store.snapshot().connected());
            store.dispatch(Intent::SetDraftText { thread_id: "thread".into(), text: "preserved".into() })
                .unwrap().wait().await.unwrap();
            // Leave every automatic read pending on the old transport.
            let mut methods = std::collections::BTreeSet::new();
            for _ in 0..3 {
                let line = tokio::time::timeout(Duration::from_secs(2), old.read_line()).await
                    .expect("Connected must reload without native intents").unwrap().unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                methods.insert(request["method"].as_str().unwrap().to_owned());
            }
            assert_eq!(methods, ["host/thread/list", "host/thread/read", "model/list"].map(str::to_owned).into());
            let server = async {
                if recovery != "disconnected" {
                    for _ in 0..2 {
                        assert!(old.read_line().await.unwrap().is_some());
                    }
                    if recovery == "closed" { first.close(); }
                }
                assert!(!matches!(old.read_line().await, Ok(Some(_))));
                let next = host.accept().await.unwrap().unwrap().authorize(&trust).unwrap();
                let (read, write) = tokio::io::split(next.accept_stream().await.unwrap());
                let mut reader = JsonlReader::new(read);
                let mut writer = JsonlWriter::new(write);
                assert_eq!(reader.read_line().await.unwrap().as_deref(), Some(""));
                let mut requests = std::collections::BTreeMap::new();
                for _ in 0..3 {
                    let request: Value = serde_json::from_str(&reader.read_line().await.unwrap().unwrap()).unwrap();
                    assert!(requests.insert(request["method"].as_str().unwrap().to_owned(), request).is_none());
                }
                let list = &requests["host/thread/list"];
                assert_eq!(list["params"]["projectLimit"], 15);
                assert_eq!(list["params"]["chatLimit"], 25);
                assert_eq!(list["params"]["projectThreadLimits"]["project"], 35);
                assert_eq!(list["params"]["searchTerm"], "retained search");
                let open = &requests["host/thread/read"];
                assert_eq!(open["params"]["threadId"], "thread");
                // Finish the conversation before the lists; no reload invalidates another.
                writer.write_line(&json!({"id":open["id"], "result":{"thread":{"id":"thread","turns":[{"id":"turn","items":[{"id":"answer","type":"agentMessage","text":"after reconnect"}]}]}}}).to_string()).await.unwrap();
                writer.write_line(&json!({"id":requests["model/list"]["id"], "result":{"data":[{"id":"fresh-model","model":"fresh-model","displayName":"Fresh","defaultReasoningEffort":"medium","supportedReasoningEfforts":[]}],"nextCursor":null}}).to_string()).await.unwrap();
                writer.write_line(&json!({"id":list["id"], "result":{"data":[{"id":"thread","name":"reloaded"}],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false}}).to_string()).await.unwrap();
                assert!(!matches!(reader.read_line().await, Ok(Some(_))));
                next.close();
            };
            let client = async {
                if recovery == "disconnected" { store.store.disconnect().await.unwrap(); }
                store.reconnect(connection()).await.unwrap();
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
    async fn connected_refresh_retains_transport_navigation_and_draft_after_error() {
        use crate::peer::{JsonlReader, JsonlWriter};
        use crate::transport::Trust;
        use serde_json::{Value, json};
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
                let (connected, incoming) = tokio::join!(store.reconnect(connection()), host.accept());
                connected.unwrap();
                let session = incoming.unwrap().unwrap().authorize(&trust).unwrap();
                let (read, write) = tokio::io::split(session.accept_stream().await.unwrap());
                let mut reader = JsonlReader::new(read);
                let mut writer = JsonlWriter::new(write);
                assert_eq!(reader.read_line().await.unwrap().as_deref(), Some(""));
                let server = async {
                    for round in 0..(3 + usize::from(selected)) {
                        let mut requests = Vec::new();
                        for _ in 0..(1 + usize::from(selected) + usize::from(round == 0)) {
                            requests.push(serde_json::from_str::<Value>(&reader.read_line().await.unwrap().expect("refresh must retain the existing stream")).unwrap());
                        }
                        for request in requests {
                            let result = match request["method"].as_str().unwrap() {
                                "host/thread/list" => json!({"data":[{"id":"thread","name":format!("round {round}")}],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false}),
                                "host/thread/read" => json!({"thread":{"id":"thread","turns":[]}}),
                                "model/list" if round == 0 => json!({"data":[],"nextCursor":null}),
                                method => panic!("unexpected refresh request {method}"),
                            };
                            if round == 2 && selected && request["method"] == "host/thread/read" { continue; }
                            let response = if round == 1 && request["method"] == "host/thread/list" {
                                json!({"id":request["id"],"error":{"code":-32000,"message":"temporary read error"}})
                            } else { json!({"id":request["id"],"result":result}) };
                            writer.write_line(&response.to_string()).await.unwrap();
                        }
                    }
                    assert!(!matches!(reader.read_line().await, Ok(Some(_))));
                };
                let client = async {
                    let mut updates = store.store.subscribe();
                    loop {
                        let ready = { let snapshot = updates.borrow_and_update(); snapshot.threads.is_some() && (!selected || snapshot.conversations.contains_key("thread")) };
                        if ready { break; }
                        updates.changed().await.unwrap();
                    }
                    store.dispatch(Intent::SetDraftText { thread_id: "thread".into(), text: "preserved".into() }).unwrap().wait().await.unwrap();
                    let before = store.snapshot();
                    assert!(store.reconnect(connection()).await.is_err());
                    assert!(store.snapshot().connected());
                    assert!(store.snapshot().error.as_ref().unwrap().contains("temporary read error"));
                    if selected {
                        assert!(store.reconnect(connection()).await.unwrap_err().to_string().contains("timed out"));
                        assert!(store.snapshot().connected());
                    }
                    store.reconnect(connection()).await.unwrap();
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
