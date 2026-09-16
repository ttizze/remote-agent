//! Adapter for recorded provider payloads in Store scheduling tests. Requests
//! observed by each test are the real Bex wire requests; only provider snapshots
//! and events are converted at this simulated Host boundary.
use agent_core::{
    models::ThreadResponse,
    peer::{JsonlReader, JsonlWriter},
    session::{SessionChange, SessionRef},
    state::Snapshot,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::io::{DuplexStream, WriteHalf};
#[path = "../../../host-daemon/src/host_rpc/provider_events.rs"]
mod provider_events;
#[derive(Default)]
struct State {
    requests: HashMap<String, Value>,
    sessions: HashMap<String, (uuid::Uuid, u64)>,
    threads: HashMap<String, Value>,
}
pub struct Reader {
    incoming: tokio::sync::mpsc::Receiver<Result<String, std::io::Error>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Reader {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub struct Writer {
    state: Arc<Mutex<State>>,
    output: Arc<tokio::sync::Mutex<JsonlWriter<WriteHalf<DuplexStream>>>>,
}
pub fn pair(stream: DuplexStream, snapshot: &Snapshot) -> (Reader, Writer) {
    let (read, write) = tokio::io::split(stream);
    let state = Arc::new(Mutex::new(State {
        threads: snapshot
            .conversations
            .iter()
            .map(|(id, thread)| (id.clone(), json!({"thread":thread})))
            .collect(),
        ..Default::default()
    }));
    let output = Arc::new(tokio::sync::Mutex::new(JsonlWriter::new(write)));
    let (send, incoming) = tokio::sync::mpsc::channel(64);
    let task_state = state.clone();
    let task_output = output.clone();
    let task = tokio::spawn(async move {
        let mut raw = JsonlReader::new(read);
        while let Ok(Some(line)) = raw.read_line().await {
            let value: Value = serde_json::from_str(&line).unwrap();
            task_state
                .lock()
                .unwrap()
                .requests
                .insert(value["id"].to_string(), value.clone());
            if value["method"] == "host/session/close" {
                if task_output
                    .lock()
                    .await
                    .write_line(&json!({"id":value["id"],"result":{}}).to_string())
                    .await
                    .is_err()
                {
                    break;
                }
            } else if send.send(Ok(line)).await.is_err() {
                break;
            }
        }
    });
    (Reader { incoming, task }, Writer { state, output })
}
impl Reader {
    pub async fn read_line(&mut self) -> Result<Option<String>, std::io::Error> {
        self.incoming.recv().await.transpose()
    }
}
impl Writer {
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
    pub async fn write_line(&mut self, line: &str) -> Result<(), std::io::Error> {
        let mut value: Value = serde_json::from_str(line)?;
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
                if let Some(request) = state.requests.get(&value["id"].to_string())
                    && request["method"] == "host/session/open"
                {
                    let target: SessionRef =
                        serde_json::from_value(request["params"]["session"].clone())?;
                    let subscription = uuid::Uuid::new_v4();
                    state.sessions.insert(target.thread_id(), (subscription, 0));
                    value["result"] = json!({"session":target,"subscriptionId":subscription,"revision":0,"response":response});
                }
            } else if value.get("method").is_some() {
                let method = value["method"].as_str().unwrap();
                let change = if value.get("id").is_some() {
                    let request: agent_core::client::ServerRequest =
                        serde_json::from_value(value.clone())?;
                    Some((
                        value["params"]["threadId"].as_str().unwrap().to_owned(),
                        SessionChange::Request { request },
                    ))
                } else {
                    provider_events::notification_change(method, value["params"].clone())?
                };
                if let Some((id, change)) = change {
                    if let Some(response) = state.threads.get_mut(&id) {
                        let mut decoded: ThreadResponse = serde_json::from_value(response.clone())?;
                        if let Ok(thread) = change.apply(&decoded.thread) {
                            decoded.thread = thread;
                            *response = json!(decoded);
                        }
                    }
                    let Some((subscription, revision)) = state.sessions.get_mut(&id) else {
                        return Ok(());
                    };
                    *revision += 1;
                    value = json!({"method":"host/session/update","params":{"subscriptionId":subscription,"revision":revision,"change":change}});
                }
            }
        }
        self.output
            .lock()
            .await
            .write_line(&value.to_string())
            .await
            .map_err(std::io::Error::other)
    }
}
