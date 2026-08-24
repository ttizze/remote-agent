use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt},
    sync::{broadcast, mpsc, oneshot},
    time::timeout,
};

use crate::wire::{RawResponse, RequestId, ServerEvent, ServerResponse};
use crate::{Error, RemoteError};

const DEFAULT_OUTBOUND_QUEUE: usize = 128;

pub(crate) struct RpcPeer {
    next_id: AtomicU64,
    outbound: mpsc::Sender<String>,
    pending: Arc<Mutex<HashMap<u64, PendingRequest>>>,
    events: EventSource,
    request_timeout: Duration,
}

struct PendingRequest {
    method: String,
    response: oneshot::Sender<Result<RawResponse, Error>>,
}

/// Removes a request from the correlation table if its caller stops waiting.
///
/// The pending table uses a synchronous mutex deliberately: `Drop` cannot
/// await a Tokio mutex, while cancellation cleanup must happen when the
/// request future is dropped rather than at some later response or timeout.
struct PendingCleanup {
    pending: Arc<Mutex<HashMap<u64, PendingRequest>>>,
    id: Option<u64>,
}

impl PendingCleanup {
    fn new(pending: Arc<Mutex<HashMap<u64, PendingRequest>>>, id: u64) -> Self {
        Self {
            pending,
            id: Some(id),
        }
    }
}

impl Drop for PendingCleanup {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        self.pending
            .lock()
            .expect("pending request mutex poisoned")
            .remove(&id);
    }
}

/// Owns the sole event sender independently of the peer handle.
///
/// The read loop takes the sender out when its input reaches EOF or becomes
/// malformed. Existing subscribers then observe `RecvError::Closed`, while a
/// subscription made after closure receives an already-closed receiver.
#[derive(Clone)]
struct EventSource {
    sender: Arc<Mutex<Option<broadcast::Sender<ServerEvent>>>>,
}

impl EventSource {
    fn new() -> Self {
        let (sender, _) = broadcast::channel(DEFAULT_OUTBOUND_QUEUE);
        Self {
            sender: Arc::new(Mutex::new(Some(sender))),
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        let sender = self.sender.lock().expect("event sender mutex poisoned");
        match sender.as_ref() {
            Some(sender) => sender.subscribe(),
            None => {
                let (sender, receiver) = broadcast::channel(1);
                drop(sender);
                receiver
            }
        }
    }

    fn send(&self, event: ServerEvent) {
        let sender = self.sender.lock().expect("event sender mutex poisoned");
        if let Some(sender) = sender.as_ref() {
            let _ = sender.send(event);
        }
    }

    fn close(&self) {
        self.sender
            .lock()
            .expect("event sender mutex poisoned")
            .take();
    }
}

impl RpcPeer {
    pub(crate) fn open<R, W>(reader: R, writer: W, request_timeout: Duration) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (outbound, outbound_rx) = mpsc::channel(DEFAULT_OUTBOUND_QUEUE);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let events = EventSource::new();

        tokio::spawn(write_loop(writer, outbound_rx, pending.clone()));
        tokio::spawn(read_loop(reader, pending.clone(), events.clone()));

        Self {
            next_id: AtomicU64::new(1),
            outbound,
            pending,
            events,
            request_timeout,
        }
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.events.subscribe()
    }

    pub(crate) async fn request<P, R>(&self, method: &str, params: P) -> Result<R, Error>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let params = serde_json::to_value(params)?;
        let value = self.request_json(method, params, Map::new()).await?;

        serde_json::from_value(value).map_err(|error| Error::UnexpectedResponse {
            method: method.to_owned(),
            reason: error.to_string(),
        })
    }

    pub(crate) async fn request_json(
        &self,
        method: &str,
        params: Value,
        extensions: Map<String, Value>,
    ) -> Result<Value, Error> {
        self.request_json_with_extensions(method, params, extensions)
            .await
            .map(|response| response.result)
    }

    pub(crate) async fn request_json_with_extensions(
        &self,
        method: &str,
        params: Value,
        extensions: Map<String, Value>,
    ) -> Result<RawResponse, Error> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut object = extensions;
        object.insert("id".to_owned(), json!(id));
        object.insert("method".to_owned(), json!(method));
        object.insert("params".to_owned(), params);
        let message = serde_json::to_string(&object)?;
        let (response_tx, response_rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("pending request mutex poisoned")
            .insert(
                id,
                PendingRequest {
                    method: method.to_owned(),
                    response: response_tx,
                },
            );
        let _cleanup = PendingCleanup::new(self.pending.clone(), id);

        let exchange = async {
            self.outbound
                .send(message)
                .await
                .map_err(|_| Error::ConnectionClosed("writer task stopped".to_owned()))?;
            response_rx
                .await
                .map_err(|_| Error::ConnectionClosed("reader task stopped".to_owned()))?
        };
        match timeout(self.request_timeout, exchange).await {
            Ok(response) => response,
            Err(_) => Err(Error::RequestTimeout {
                method: method.to_owned(),
            }),
        }
    }

    pub(crate) async fn notify(&self, method: &str, params: impl Serialize) -> Result<(), Error> {
        self.notify_json(method, serde_json::to_value(params)?, Map::new())
            .await
    }

    pub(crate) async fn notify_json(
        &self,
        method: &str,
        params: Value,
        extensions: Map<String, Value>,
    ) -> Result<(), Error> {
        let mut object = extensions;
        object.insert("method".to_owned(), json!(method));
        object.insert("params".to_owned(), params);
        let message = serde_json::to_string(&object)?;
        self.outbound
            .send(message)
            .await
            .map_err(|_| Error::ConnectionClosed("writer task stopped".to_owned()))
    }

    pub(crate) async fn respond_json(
        &self,
        id: RequestId,
        response: ServerResponse,
        extensions: Map<String, Value>,
    ) -> Result<(), Error> {
        let mut message_object = extensions;
        message_object.insert("id".to_owned(), serde_json::to_value(id)?);
        let response_object = serde_json::to_value(response)?;
        let Some(response_object) = response_object.as_object() else {
            return Err(Error::UnexpectedResponse {
                method: "server request response".to_owned(),
                reason: "raw response was not a JSON object".to_owned(),
            });
        };
        message_object.extend(response_object.clone());
        let message = serde_json::to_string(&Value::Object(message_object))?;
        self.outbound
            .send(message)
            .await
            .map_err(|_| Error::ConnectionClosed("writer task stopped".to_owned()))
    }
}

async fn write_loop<W>(
    mut writer: W,
    mut outbound: mpsc::Receiver<String>,
    pending: Arc<Mutex<HashMap<u64, PendingRequest>>>,
) where
    W: AsyncWrite + Unpin,
{
    while let Some(message) = outbound.recv().await {
        if let Err(error) = writer.write_all(message.as_bytes()).await {
            fail_pending(&pending, format!("write failed: {error}"));
            return;
        }
        if let Err(error) = writer.write_all(b"\n").await {
            fail_pending(&pending, format!("write failed: {error}"));
            return;
        }
        if let Err(error) = writer.flush().await {
            fail_pending(&pending, format!("flush failed: {error}"));
            return;
        }
    }
}

async fn read_loop<R>(
    reader: R,
    pending: Arc<Mutex<HashMap<u64, PendingRequest>>>,
    events: EventSource,
) where
    R: AsyncRead + Unpin,
{
    let mut lines = tokio::io::BufReader::new(reader).lines();
    let reason = loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break "stdout reached EOF".to_owned(),
            Err(error) => break format!("failed to read JSONL input: {error}"),
        };
        let message: Value = match serde_json::from_str(&line) {
            Ok(message) => message,
            Err(error) => break format!("invalid JSON: {error}"),
        };

        if let Some(method) = message.get("method").and_then(Value::as_str) {
            let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
            let extensions = message
                .as_object()
                .into_iter()
                .flat_map(|object| object.iter())
                .filter(|(key, _)| !matches!(key.as_str(), "id" | "method" | "params"))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            if let Some(id) = message.get("id") {
                match serde_json::from_value::<RequestId>(id.clone()) {
                    Ok(id) => {
                        events.send(ServerEvent::Request {
                            id,
                            method: method.to_owned(),
                            params,
                            extensions,
                        });
                    }
                    Err(error) => {
                        break format!("invalid server request id: {error}");
                    }
                }
            } else {
                events.send(ServerEvent::Notification {
                    method: method.to_owned(),
                    params,
                    extensions,
                });
            }
            continue;
        }

        let Some(id) = message.get("id").and_then(Value::as_u64) else {
            break "message had neither method nor numeric response id".to_owned();
        };
        let Some(pending_request) = pending
            .lock()
            .expect("pending request mutex poisoned")
            .remove(&id)
        else {
            continue;
        };

        let response_extensions = response_extensions(&message);
        if let Some(result) = message.get("result") {
            let _ = pending_request.response.send(Ok(RawResponse {
                result: result.clone(),
                extensions: response_extensions,
            }));
            continue;
        }

        if let Some(error) = message.get("error") {
            let (code, message, data, additional_fields) = remote_error_fields(error);
            let _ = pending_request.response.send(Err(Error::Remote {
                method: pending_request.method,
                detail: Box::new(RemoteError {
                    code,
                    message,
                    data,
                    additional_fields,
                    response_extensions,
                }),
            }));
            continue;
        }

        let _ = pending_request.response.send(Err(Error::ConnectionClosed(
            "response had neither result nor error".to_owned(),
        )));
    };

    fail_pending(&pending, reason);
    events.close();
}

fn response_extensions(message: &Value) -> Map<String, Value> {
    message
        .as_object()
        .into_iter()
        .flat_map(|object| object.iter())
        .filter(|(key, _)| !matches!(key.as_str(), "id" | "result" | "error"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn remote_error_fields(error: &Value) -> (Value, String, Option<Value>, Map<String, Value>) {
    let Some(object) = error.as_object() else {
        return (
            json!(-1),
            "unknown App Server error".to_owned(),
            None,
            Map::from_iter([(String::from("raw"), error.clone())]),
        );
    };

    let code = object.get("code").cloned().unwrap_or_else(|| json!(-1));
    let message = object
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown App Server error")
        .to_owned();
    let data = object.get("data").cloned();
    let additional_fields = object
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "code" | "message" | "data"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    (code, message, data, additional_fields)
}

fn fail_pending(pending: &Arc<Mutex<HashMap<u64, PendingRequest>>>, reason: String) {
    for (_, pending_request) in pending
        .lock()
        .expect("pending request mutex poisoned")
        .drain()
    {
        let _ = pending_request
            .response
            .send(Err(Error::ConnectionClosed(reason.clone())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};

    #[tokio::test]
    async fn correlates_out_of_order_responses() {
        let (client_io, server_io) = duplex(8 * 1024);
        let (client_reader, client_writer) = tokio::io::split(client_io);
        let (server_reader, mut server_writer) = tokio::io::split(server_io);
        let peer = Arc::new(RpcPeer::open(
            client_reader,
            client_writer,
            Duration::from_secs(1),
        ));

        let first = {
            let peer = peer.clone();
            tokio::spawn(async move { peer.request::<_, Value>("first", json!({})).await })
        };
        let second = {
            let peer = peer.clone();
            tokio::spawn(async move { peer.request::<_, Value>("second", json!({})).await })
        };

        let mut lines = BufReader::new(server_reader).lines();
        let request_a: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let request_b: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let id_a = request_a["id"].as_u64().unwrap();
        let id_b = request_b["id"].as_u64().unwrap();
        let method_a = request_a["method"].as_str().unwrap();
        let method_b = request_b["method"].as_str().unwrap();

        server_writer
            .write_all(
                format!(
                    "{}\n",
                    json!({ "id": id_b, "result": { "method": method_b } })
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        server_writer
            .write_all(
                format!(
                    "{}\n",
                    json!({ "id": id_a, "result": { "method": method_a } })
                )
                .as_bytes(),
            )
            .await
            .unwrap();

        let first = first.await.unwrap().unwrap();
        let second = second.await.unwrap().unwrap();
        assert_eq!(first["method"], "first");
        assert_eq!(second["method"], "second");
    }

    #[tokio::test]
    async fn sends_arbitrary_json_request_and_returns_raw_result() {
        let (client_io, server_io) = duplex(8 * 1024);
        let (client_reader, client_writer) = tokio::io::split(client_io);
        let (server_reader, mut server_writer) = tokio::io::split(server_io);
        let peer = Arc::new(RpcPeer::open(
            client_reader,
            client_writer,
            Duration::from_secs(1),
        ));

        let request = {
            let peer = peer.clone();
            tokio::spawn(async move {
                peer.request_json(
                    "item/commandExecution/requestApproval",
                    json!({
                        "command": ["cargo", "test"],
                        "futureField": {"enabled": true},
                    }),
                    Map::from_iter([(String::from("jsonrpc"), json!("2.0"))]),
                )
                .await
            })
        };

        let mut lines = BufReader::new(server_reader).lines();
        let sent: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let id = sent["id"].as_u64().unwrap();
        assert_eq!(sent["method"], "item/commandExecution/requestApproval");
        assert_eq!(sent["jsonrpc"], "2.0");
        assert_eq!(
            sent["params"],
            json!({
                "command": ["cargo", "test"],
                "futureField": {"enabled": true},
            })
        );

        server_writer
            .write_all(
                format!(
                    "{}\n",
                    json!({
                        "id": id,
                        "result": {
                            "decision": "accept",
                            "newResultField": [1, 2, 3],
                        },
                    })
                )
                .as_bytes(),
            )
            .await
            .unwrap();

        assert_eq!(
            request.await.unwrap().unwrap(),
            json!({
                "decision": "accept",
                "newResultField": [1, 2, 3],
            })
        );
    }

    #[tokio::test]
    async fn preserves_numeric_remote_error_code_and_unknown_fields() {
        let (client_io, server_io) = duplex(8 * 1024);
        let (client_reader, client_writer) = tokio::io::split(client_io);
        let (server_reader, mut server_writer) = tokio::io::split(server_io);
        let peer = RpcPeer::open(client_reader, client_writer, Duration::from_secs(1));
        let request = tokio::spawn(async move {
            peer.request_json("future/method", json!({"input": true}), Map::new())
                .await
        });

        let mut lines = BufReader::new(server_reader).lines();
        let sent: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let id = sent["id"].as_u64().unwrap();
        server_writer
            .write_all(
                format!(
                    "{}\n",
                    json!({
                        "id": id,
                        "error": {
                            "code": -32001,
                            "message": "approval unavailable",
                            "data": {"retryable": true},
                            "futureField": [1, 2, 3],
                        },
                    })
                )
                .as_bytes(),
            )
            .await
            .unwrap();

        let error = request.await.unwrap().unwrap_err();
        match error {
            Error::Remote { method, detail } => {
                assert_eq!(method, "future/method");
                assert_eq!(detail.code, json!(-32001));
                assert_eq!(detail.message, "approval unavailable");
                assert_eq!(detail.data, Some(json!({"retryable": true})));
                assert_eq!(
                    detail.additional_fields.get("futureField"),
                    Some(&json!([1, 2, 3]))
                );
                let display = Error::Remote {
                    method: "future/method".to_owned(),
                    detail,
                }
                .to_string();
                assert!(display.contains("future/method"));
                assert!(display.contains("-32001"));
                assert!(display.contains("approval unavailable"));
            }
            other => panic!("expected remote error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn serializes_raw_result_and_error_server_responses() {
        let (client_io, server_io) = duplex(8 * 1024);
        let (client_reader, client_writer) = tokio::io::split(client_io);
        let (server_reader, _server_writer) = tokio::io::split(server_io);
        let peer = RpcPeer::open(client_reader, client_writer, Duration::from_secs(1));
        let mut lines = BufReader::new(server_reader).lines();

        peer.respond_json(
            RequestId::String("approval-1".to_owned()),
            ServerResponse::Result {
                result: json!({"decision": "accept", "future": {"kept": true}}),
            },
            Map::from_iter([(String::from("jsonrpc"), json!("2.0"))]),
        )
        .await
        .unwrap();
        let result: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(
            result,
            json!({
                "id": "approval-1",
                "jsonrpc": "2.0",
                "result": {"decision": "accept", "future": {"kept": true}},
            })
        );

        peer.respond_json(
            RequestId::String("approval-2".to_owned()),
            ServerResponse::Error {
                error: json!({
                    "code": -32002,
                    "message": "declined",
                    "futureErrorField": {"preserved": true},
                }),
            },
            Map::new(),
        )
        .await
        .unwrap();
        let error: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(
            error,
            json!({
                "id": "approval-2",
                "error": {
                    "code": -32002,
                    "message": "declined",
                    "futureErrorField": {"preserved": true},
                },
            })
        );
    }

    #[tokio::test]
    async fn emits_notifications_and_server_requests() {
        let (client_io, mut server_io) = duplex(8 * 1024);
        let (client_reader, client_writer) = tokio::io::split(client_io);
        let peer = RpcPeer::open(client_reader, client_writer, Duration::from_secs(1));
        let mut events = peer.subscribe();

        server_io
            .write_all(
                b"{\"method\":\"turn/started\",\"params\":{\"turnId\":\"t1\"},\"future\":true}\n",
            )
            .await
            .unwrap();
        server_io
            .write_all(b"{\"id\":\"approval-1\",\"method\":\"item/commandExecution/requestApproval\",\"params\":{},\"jsonrpc\":\"2.0\"}\n")
            .await
            .unwrap();

        assert_eq!(
            events.recv().await.unwrap(),
            ServerEvent::Notification {
                method: "turn/started".to_owned(),
                params: json!({ "turnId": "t1" }),
                extensions: Map::from_iter([(String::from("future"), json!(true))]),
            }
        );
        assert_eq!(
            events.recv().await.unwrap(),
            ServerEvent::Request {
                id: RequestId::String("approval-1".to_owned()),
                method: "item/commandExecution/requestApproval".to_owned(),
                params: json!({}),
                extensions: Map::from_iter([(String::from("jsonrpc"), json!("2.0"))]),
            }
        );
    }

    #[tokio::test]
    async fn times_out_and_removes_pending_request() {
        let (client_io, _server_io) = duplex(1024);
        let (client_reader, client_writer) = tokio::io::split(client_io);
        let peer = RpcPeer::open(client_reader, client_writer, Duration::from_millis(10));

        let error = peer
            .request::<_, Value>("never-responds", json!({}))
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            Error::RequestTimeout { ref method } if method == "never-responds"
        ));
        assert!(
            peer.pending
                .lock()
                .expect("pending request mutex poisoned")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn request_deadline_includes_waiting_for_outbound_capacity() {
        let (client_io, server_io) = duplex(1);
        let (client_reader, client_writer) = tokio::io::split(client_io);
        let peer = Arc::new(RpcPeer::open(
            client_reader,
            client_writer,
            Duration::from_millis(20),
        ));
        let mut requests = tokio::task::JoinSet::new();
        for _ in 0..140 {
            let peer = peer.clone();
            requests
                .spawn(async move { peer.request_json("blocked", json!({}), Map::new()).await });
        }

        tokio::time::timeout(Duration::from_secs(1), async {
            while let Some(request) = requests.join_next().await {
                assert!(matches!(
                    request.unwrap(),
                    Err(Error::RequestTimeout { ref method }) if method == "blocked"
                ));
            }
        })
        .await
        .expect("every queued request must observe its deadline");
        assert!(
            peer.pending
                .lock()
                .expect("pending request mutex poisoned")
                .is_empty()
        );
        drop(server_io);
    }

    #[tokio::test]
    async fn cancellation_removes_pending_request() {
        let (client_io, server_io) = duplex(8 * 1024);
        let (client_reader, client_writer) = tokio::io::split(client_io);
        let (server_reader, _server_writer) = tokio::io::split(server_io);
        let peer = Arc::new(RpcPeer::open(
            client_reader,
            client_writer,
            Duration::from_secs(1),
        ));
        let request = {
            let peer = peer.clone();
            tokio::spawn(async move { peer.request_json("cancelled", json!({}), Map::new()).await })
        };

        let mut lines = BufReader::new(server_reader).lines();
        lines.next_line().await.unwrap().unwrap();
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        assert!(
            peer.pending
                .lock()
                .expect("pending request mutex poisoned")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn accepts_messages_larger_than_the_removed_four_mibibyte_limit() {
        let (client_io, server_io) = duplex(8 * 1024);
        let (client_reader, client_writer) = tokio::io::split(client_io);
        let (server_reader, mut server_writer) = tokio::io::split(server_io);
        let peer = Arc::new(RpcPeer::open(
            client_reader,
            client_writer,
            Duration::from_secs(1),
        ));
        let request = {
            let peer = peer.clone();
            tokio::spawn(async move { peer.request::<_, Value>("waiting", json!({})).await })
        };

        let mut lines = BufReader::new(server_reader).lines();
        lines.next_line().await.unwrap().unwrap();
        let content = "x".repeat(4 * 1024 * 1024 + 1);
        let response = serde_json::to_vec(&json!({
            "id": 1,
            "result": { "content": content },
        }))
        .unwrap();
        server_writer.write_all(&response).await.unwrap();
        server_writer.write_all(b"\n").await.unwrap();

        let result = request.await.unwrap().unwrap();
        assert_eq!(
            result.get("content").and_then(Value::as_str).unwrap().len(),
            4 * 1024 * 1024 + 1
        );
    }

    #[tokio::test]
    async fn malformed_json_closes_pending_requests_and_event_source() {
        let (client_io, server_io) = duplex(8 * 1024);
        let (client_reader, client_writer) = tokio::io::split(client_io);
        let (server_reader, mut server_writer) = tokio::io::split(server_io);
        let peer = Arc::new(RpcPeer::open(
            client_reader,
            client_writer,
            Duration::from_secs(1),
        ));
        let mut events = peer.subscribe();
        let request = {
            let peer = peer.clone();
            tokio::spawn(async move { peer.request::<_, Value>("waiting", json!({})).await })
        };

        let mut lines = BufReader::new(server_reader).lines();
        lines.next_line().await.unwrap().unwrap();
        server_writer.write_all(b"{not-json}\n").await.unwrap();

        let error = request.await.unwrap().unwrap_err();
        assert!(
            matches!(error, Error::ConnectionClosed(reason) if reason.contains("invalid JSON"))
        );
        assert!(matches!(
            events.recv().await,
            Err(broadcast::error::RecvError::Closed)
        ));
    }

    #[tokio::test]
    async fn eof_closes_pending_requests_and_event_source() {
        let (client_io, server_io) = duplex(8 * 1024);
        let (client_reader, client_writer) = tokio::io::split(client_io);
        let (server_reader, server_writer) = tokio::io::split(server_io);
        let peer = Arc::new(RpcPeer::open(
            client_reader,
            client_writer,
            Duration::from_secs(1),
        ));
        let mut events = peer.subscribe();
        let request = {
            let peer = peer.clone();
            tokio::spawn(async move { peer.request::<_, Value>("waiting", json!({})).await })
        };

        let mut lines = BufReader::new(server_reader).lines();
        lines.next_line().await.unwrap().unwrap();
        drop(lines);
        drop(server_writer);

        let error = request.await.unwrap().unwrap_err();
        assert!(matches!(error, Error::ConnectionClosed(reason) if reason.contains("EOF")));
        assert!(matches!(
            events.recv().await,
            Err(broadcast::error::RecvError::Closed)
        ));
    }
}
