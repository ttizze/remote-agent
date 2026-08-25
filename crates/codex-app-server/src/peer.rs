use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use host_protocol::{
    JsonlReader, JsonlWriter, RpcMessageKind, classify_message, rewrite_top_level_id,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{broadcast, mpsc, oneshot},
    time::timeout,
};

use crate::Error;

const DEFAULT_OUTBOUND_QUEUE: usize = 128;

/// A single raw JSONL peer backed by one Codex App Server child process.
///
/// The peer only parses the message envelope needed for routing. All nested
/// JSON, including params, result, error, and unknown extension members,
/// remains in the original source line.
pub(crate) struct RpcPeer {
    next_id: AtomicU64,
    outbound: mpsc::Sender<String>,
    pending: Arc<Mutex<HashMap<u64, PendingRequest>>>,
    events: EventSource,
    closed: Arc<AtomicBool>,
    request_timeout: Duration,
}

struct PendingRequest {
    method: String,
    original_id: String,
    response: oneshot::Sender<Result<String, Error>>,
}

/// Removes a request from the correlation table when the request future is
/// dropped. This is what makes cancellation different from merely stopping
/// to poll the response: a later Codex response becomes an unknown response
/// and is ignored instead of being delivered to another request.
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

/// Keeps a broadcast sender independently of the peer handle. Once the
/// reader or writer terminates, existing subscribers observe `Closed` and a
/// later subscription receives an already-closed receiver.
#[derive(Clone)]
struct EventSource {
    sender: Arc<Mutex<Option<broadcast::Sender<String>>>>,
}

impl EventSource {
    fn new() -> Self {
        let (sender, _) = broadcast::channel(DEFAULT_OUTBOUND_QUEUE);
        Self {
            sender: Arc::new(Mutex::new(Some(sender))),
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<String> {
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

    fn send(&self, line: String) {
        let sender = self.sender.lock().expect("event sender mutex poisoned");
        if let Some(sender) = sender.as_ref() {
            let _ = sender.send(line);
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
        let closed = Arc::new(AtomicBool::new(false));

        tokio::spawn(write_loop(
            writer,
            outbound_rx,
            pending.clone(),
            events.clone(),
            closed.clone(),
        ));
        tokio::spawn(read_loop(
            reader,
            pending.clone(),
            events.clone(),
            closed.clone(),
        ));

        Self {
            next_id: AtomicU64::new(1),
            outbound,
            pending,
            events,
            closed,
            request_timeout,
        }
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<String> {
        self.events.subscribe()
    }

    /// Sends a raw JSON-RPC request and returns the raw response line with
    /// the caller's original top-level id restored.
    pub(crate) async fn request_raw(&self, line: &str) -> Result<String, Error> {
        let parsed = classify_message(line).map_err(invalid_message)?;
        if parsed.kind() != RpcMessageKind::Request {
            return Err(Error::InvalidMessage(
                "Codex request must contain method and id".to_owned(),
            ));
        }
        let original_id = parsed.raw_id().map(str::to_owned).ok_or_else(|| {
            Error::InvalidMessage("Codex request is missing its top-level id".to_owned())
        })?;
        let method = parsed.method().map(str::to_owned).ok_or_else(|| {
            Error::InvalidMessage("Codex request is missing its method".to_owned())
        })?;

        let (response_tx, response_rx) = oneshot::channel();
        let upstream_id = self.reserve_request(PendingRequest {
            method: method.clone(),
            original_id: original_id.clone(),
            response: response_tx,
        });
        let upstream_id_text = upstream_id.to_string();
        let outbound_line = if original_id == upstream_id_text {
            line.to_owned()
        } else {
            match rewrite_top_level_id(line, &upstream_id_text) {
                Ok(line) => line,
                Err(error) => {
                    self.pending
                        .lock()
                        .expect("pending request mutex poisoned")
                        .remove(&upstream_id);
                    return Err(invalid_message(error));
                }
            }
        };
        let _cleanup = PendingCleanup::new(self.pending.clone(), upstream_id);

        let exchange = async {
            self.enqueue(outbound_line).await?;
            response_rx
                .await
                .map_err(|_| Error::ConnectionClosed("reader task stopped".to_owned()))?
        };
        match timeout(self.request_timeout, exchange).await {
            Ok(response) => response,
            Err(_) => Err(Error::RequestTimeout { method }),
        }
    }

    /// Sends a notification or response exactly as supplied, after checking
    /// its JSON-RPC envelope. Requests must go through `request_raw` so the
    /// peer can correlate and restore their caller-owned id.
    pub(crate) async fn send_raw(&self, line: &str) -> Result<(), Error> {
        let parsed = classify_message(line).map_err(invalid_message)?;
        if parsed.kind() == RpcMessageKind::Request {
            return Err(Error::InvalidMessage(
                "raw requests must be sent through request_raw".to_owned(),
            ));
        }
        self.enqueue(line.to_owned()).await
    }

    async fn enqueue(&self, line: String) -> Result<(), Error> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::ConnectionClosed("peer is closed".to_owned()));
        }
        self.outbound
            .send(line)
            .await
            .map_err(|_| Error::ConnectionClosed("writer task stopped".to_owned()))
    }

    fn reserve_request(&self, pending_request: PendingRequest) -> u64 {
        loop {
            // Zero is deliberately skipped. It is valid JSON-RPC, but using
            // positive ids keeps the process-owned namespace unambiguous.
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            if id == 0 {
                continue;
            }
            let mut pending = self.pending.lock().expect("pending request mutex poisoned");
            if pending.contains_key(&id) {
                continue;
            }
            pending.insert(id, pending_request);
            return id;
        }
    }
}

fn invalid_message(error: impl std::fmt::Display) -> Error {
    Error::InvalidMessage(error.to_string())
}

async fn write_loop<W>(
    writer: W,
    mut outbound: mpsc::Receiver<String>,
    pending: Arc<Mutex<HashMap<u64, PendingRequest>>>,
    events: EventSource,
    closed: Arc<AtomicBool>,
) where
    W: AsyncWrite + Unpin,
{
    let mut writer = JsonlWriter::new(writer);
    let reason = loop {
        let Some(line) = outbound.recv().await else {
            break "outbound queue closed".to_owned();
        };
        if let Err(error) = writer.write_line(&line).await {
            break format!("flush failed: {error}");
        }
    };
    terminate_peer(&pending, &events, &closed, reason);
}

async fn read_loop<R>(
    reader: R,
    pending: Arc<Mutex<HashMap<u64, PendingRequest>>>,
    events: EventSource,
    closed: Arc<AtomicBool>,
) where
    R: AsyncRead + Unpin,
{
    let mut lines = JsonlReader::new(reader);
    let reason = loop {
        let line = match lines.read_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break "stdout reached EOF".to_owned(),
            Err(error) => break format!("failed to read JSONL input: {error}"),
        };
        let message = match classify_message(&line) {
            Ok(message) => message,
            Err(error) => break format!("invalid JSONL message: {error}"),
        };

        match message.kind() {
            RpcMessageKind::Request | RpcMessageKind::Notification => {
                events.send(line);
            }
            RpcMessageKind::Response => {
                // App Server responses to this peer use numeric ids allocated
                // above. A response with any other id is a late or unrelated
                // response and is intentionally ignored.
                let Some(raw_id) = message.raw_id() else {
                    break "response had no id".to_owned();
                };
                let Ok(id) = serde_json::from_str::<u64>(raw_id) else {
                    continue;
                };
                let pending_request = pending
                    .lock()
                    .expect("pending request mutex poisoned")
                    .remove(&id);
                let Some(pending_request) = pending_request else {
                    continue;
                };
                let response = if pending_request.original_id == raw_id {
                    Ok(line)
                } else {
                    rewrite_top_level_id(&line, &pending_request.original_id).map_err(|error| {
                        format!(
                            "could not restore response id for {}: {error}",
                            pending_request.method
                        )
                    })
                };
                match response {
                    Ok(response) => {
                        let _ = pending_request.response.send(Ok(response));
                    }
                    Err(reason) => {
                        let _ = pending_request
                            .response
                            .send(Err(Error::InvalidMessage(reason.clone())));
                        break reason;
                    }
                }
            }
        }
    };

    terminate_peer(&pending, &events, &closed, reason);
}

fn fail_pending(pending: &Arc<Mutex<HashMap<u64, PendingRequest>>>, reason: String) {
    for pending_request in pending
        .lock()
        .expect("pending request mutex poisoned")
        .drain()
        .map(|(_, request)| request)
    {
        let _ = pending_request
            .response
            .send(Err(Error::ConnectionClosed(reason.clone())));
    }
}

fn terminate_peer(
    pending: &Arc<Mutex<HashMap<u64, PendingRequest>>>,
    events: &EventSource,
    closed: &Arc<AtomicBool>,
    reason: String,
) {
    closed.store(true, Ordering::Release);
    fail_pending(pending, reason);
    events.close();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};

    fn make_peer() -> (
        Arc<RpcPeer>,
        tokio::io::ReadHalf<tokio::io::DuplexStream>,
        tokio::io::WriteHalf<tokio::io::DuplexStream>,
    ) {
        let (client_io, server_io) = duplex(32 * 1024);
        let (client_reader, client_writer) = tokio::io::split(client_io);
        let (server_reader, server_writer) = tokio::io::split(server_io);
        (
            Arc::new(RpcPeer::open(
                client_reader,
                client_writer,
                Duration::from_secs(1),
            )),
            server_reader,
            server_writer,
        )
    }

    async fn read_line(
        reader: &mut BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    ) -> String {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .expect("server read should succeed");
        if line.ends_with('\n') {
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
        }
        line
    }

    #[tokio::test]
    async fn correlates_responses_and_restores_original_raw_ids() {
        let (peer, server_reader, mut server_writer) = make_peer();
        let mut lines = BufReader::new(server_reader);
        let first = {
            let peer = peer.clone();
            tokio::spawn(async move {
                peer.request_raw(
                    r#"{"id":"mobile-a","method":"first","params":{"nested":{"id":1}},"future":{"keep":true}}"#,
                )
                .await
            })
        };
        let second = {
            let peer = peer.clone();
            tokio::spawn(async move {
                peer.request_raw(r#"{"id":42,"method":"second","params":{"unknown":[1,2,3]}}"#)
                    .await
            })
        };

        let request_a: Value = serde_json::from_str(&read_line(&mut lines).await).unwrap();
        let request_b: Value = serde_json::from_str(&read_line(&mut lines).await).unwrap();
        let id_a = request_a["id"].as_u64().unwrap();
        let id_b = request_b["id"].as_u64().unwrap();
        assert_ne!(id_a, id_b);
        assert_eq!(request_a["params"]["nested"]["id"], 1);
        assert_eq!(request_a["future"]["keep"], true);
        assert_eq!(request_b["params"]["unknown"], json!([1, 2, 3]));

        server_writer
            .write_all(
                format!(
                    "{{\"id\":{id_b},\"result\":{{\"method\":\"second\",\"futureResult\":{{\"id\":99}}}},\"unknown\":[true]}}\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        server_writer
            .write_all(
                format!(
                    "{{\"id\":{id_a},\"error\":{{\"code\":-1,\"message\":\"nope\",\"futureError\":{{\"id\":7}}}},\"extension\":{{\"keep\":true}}}}\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();

        let first: Value = serde_json::from_str(&first.await.unwrap().unwrap()).unwrap();
        let second: Value = serde_json::from_str(&second.await.unwrap().unwrap()).unwrap();
        assert_eq!(first["id"], "mobile-a");
        assert_eq!(first["error"]["futureError"]["id"], 7);
        assert_eq!(first["extension"]["keep"], true);
        assert_eq!(second["id"], 42);
        assert_eq!(second["result"]["futureResult"]["id"], 99);
        assert_eq!(second["unknown"], json!([true]));
    }

    #[tokio::test]
    async fn publishes_raw_notifications_and_server_requests() {
        let (peer, server_reader, mut server_writer) = make_peer();
        let mut events = peer.subscribe();
        drop(server_reader);
        let notification = r#" {"method":"item/started","params":{"future":{"id":1}}} "#;
        let request = r#"{"id":"approval-1","method":"item/approval","params":{"opaque":[1,2]}}"#;
        server_writer
            .write_all(format!("{notification}\n{request}\n").as_bytes())
            .await
            .unwrap();

        assert_eq!(events.recv().await.unwrap(), notification);
        assert_eq!(events.recv().await.unwrap(), request);
    }

    #[tokio::test]
    async fn sends_validated_notification_and_response_without_rewriting() {
        let (peer, server_reader, _server_writer) = make_peer();
        let mut lines = BufReader::new(server_reader);
        let notification = r#" {"method":"event","params":{"future":true}} "#;
        let response = r#"{"id":"approval","result":{"future":[1,2]}}"#;
        peer.send_raw(notification).await.unwrap();
        peer.send_raw(response).await.unwrap();
        assert_eq!(read_line(&mut lines).await, notification);
        assert_eq!(read_line(&mut lines).await, response);
        assert!(
            peer.send_raw(r#"{"id":1,"method":"not-a-response"}"#)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn ignores_unknown_and_late_responses() {
        let (peer, server_reader, mut server_writer) = make_peer();
        let mut lines = BufReader::new(server_reader);
        let request = {
            let peer = peer.clone();
            tokio::spawn(async move {
                peer.request_raw(r#"{"id":"caller","method":"wait","params":{}}"#)
                    .await
            })
        };
        let sent: Value = serde_json::from_str(&read_line(&mut lines).await).unwrap();
        let id = sent["id"].as_u64().unwrap();
        server_writer
            .write_all(b"{\"id\":999,\"result\":{\"late\":true}}\n")
            .await
            .unwrap();
        server_writer
            .write_all(format!("{{\"id\":{id},\"result\":{{\"ok\":true}}}}\n").as_bytes())
            .await
            .unwrap();
        let response: Value = serde_json::from_str(&request.await.unwrap().unwrap()).unwrap();
        assert_eq!(response["id"], "caller");
        assert_eq!(response["result"]["ok"], true);
    }

    #[tokio::test]
    async fn timeout_removes_pending_request() {
        let (client_io, server_io) = duplex(32 * 1024);
        let (client_reader, client_writer) = tokio::io::split(client_io);
        let (server_reader, _server_writer) = tokio::io::split(server_io);
        let peer = Arc::new(RpcPeer::open(
            client_reader,
            client_writer,
            Duration::from_millis(10),
        ));
        let mut lines = BufReader::new(server_reader);
        let error = peer
            .request_raw(r#"{"id":"timeout","method":"blocked","params":{}}"#)
            .await
            .unwrap_err();
        let _ = read_line(&mut lines).await;
        assert!(matches!(error, Error::RequestTimeout { method } if method == "blocked"));
    }

    #[tokio::test]
    async fn cancellation_removes_pending_request_and_late_response_is_ignored() {
        let (peer, server_reader, mut server_writer) = make_peer();
        let mut lines = BufReader::new(server_reader);
        let cancelled = {
            let peer = peer.clone();
            tokio::spawn(async move {
                peer.request_raw(r#"{"id":"cancelled","method":"cancel","params":{}}"#)
                    .await
            })
        };
        let sent: Value = serde_json::from_str(&read_line(&mut lines).await).unwrap();
        let cancelled_id = sent["id"].as_u64().unwrap();
        cancelled.abort();
        let _ = cancelled.await;

        server_writer
            .write_all(
                format!("{{\"id\":{cancelled_id},\"result\":{{\"late\":true}}}}\n").as_bytes(),
            )
            .await
            .unwrap();
        let next = {
            let peer = peer.clone();
            tokio::spawn(async move {
                peer.request_raw(r#"{"id":"next","method":"next","params":{}}"#)
                    .await
            })
        };
        let sent: Value = serde_json::from_str(&read_line(&mut lines).await).unwrap();
        let next_id = sent["id"].as_u64().unwrap();
        server_writer
            .write_all(format!("{{\"id\":{next_id},\"result\":{{\"ok\":true}}}}\n").as_bytes())
            .await
            .unwrap();
        let response: Value = serde_json::from_str(&next.await.unwrap().unwrap()).unwrap();
        assert_eq!(response["id"], "next");
        assert_eq!(response["result"]["ok"], true);
    }
}
