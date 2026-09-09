//! Transport-independent, bidirectional JSONL with one ordered receive stream.
use host_protocol::{
    JsonlReader, JsonlWriter, RpcMessageKind, classify_message, raw_object, rewrite_top_level_id,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::value::RawValue;
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{Semaphore, broadcast, mpsc, oneshot, watch},
};
use tokio_util::sync::{CancellationToken, DropGuard};

const QUEUE_CAPACITY: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    #[error("invalid JSONL message: {0}")]
    InvalidMessage(String),
    #[error("Invalid {method} response: {reason}")]
    InvalidResponse {
        method: String,
        reason: String,
        raw: String,
        sequence: Option<u64>,
    },
    #[error("connection closed: {0}")]
    ConnectionClosed(String),
    #[error("request {method} timed out")]
    RequestTimeout { method: String, id: u64 },
    #[error("request ID space exhausted")]
    RequestIdExhausted,
    #[error("remote RPC error: {error}")]
    Remote {
        error: String,
        sequence: Option<u64>,
    },
}
/// Wire position shared by replies and the receive stream.
#[derive(Clone, Debug)]
pub struct Reply<T> {
    pub sequence: u64,
    pub value: T,
}
#[derive(Clone, Debug)]
pub enum PeerEvent {
    /// Notifications and server requests retain their complete envelopes.
    Message(Reply<Arc<str>>),
    /// A response marker lets Store order a typed completion with surrounding events.
    Response {
        sequence: u64,
        request_id: Option<u64>,
        method: Option<Arc<str>>,
    },
    Closed(String),
}
struct PreparedRequest {
    original_id: String,
    method: String,
    id: u64,
    line: String,
}
pin_project_lite::pin_project! {
    /// A raw request's assigned wire ID is available before its first poll.
    /// Routers use it for notifications that refer to an outstanding request.
    pub struct Request<F> {
        pub(crate) wire_id: Option<u64>,
        #[pin]
        pub(crate) response: F,
    }
}
impl<F> Request<F> {
    /// `None` means preparation failed; awaiting the request returns that error.
    pub fn wire_id(&self) -> Option<u64> {
        self.wire_id
    }
}
impl<F: Future> Future for Request<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().response.poll(cx)
    }
}
struct Outbound {
    line: String,
    written: oneshot::Sender<()>,
}
struct Pending {
    original_id: String,
    method: Arc<str>,
    complete: oneshot::Sender<Result<Reply<String>, PeerError>>,
}
struct State {
    pending: HashMap<u64, Pending>,
    closed: Option<String>,
    events: Option<broadcast::Sender<PeerEvent>>,
    initial: Option<broadcast::Receiver<PeerEvent>>,
}
pub struct RpcPeer {
    next_id: AtomicU64,
    outbound: mpsc::Sender<Outbound>,
    state: Arc<Mutex<State>>,
    permits: Arc<Semaphore>,
    stop: CancellationToken,
    _close_on_drop: DropGuard,
    request_timeout: Duration,
    writer_done: watch::Receiver<Option<Result<(), String>>>,
}

impl RpcPeer {
    pub fn open<R, W>(
        reader: JsonlReader<R>,
        writer: W,
        request_timeout: Duration,
        max_requests: usize,
    ) -> Result<Self, PeerError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let maximum = reader.max_message_bytes();
        if maximum == 0 {
            return Err(PeerError::InvalidMessage(
                "max_message_bytes must be positive".into(),
            ));
        }
        if max_requests == 0 {
            return Err(PeerError::InvalidMessage(
                "max_requests must be positive".into(),
            ));
        }
        let (events, initial) = broadcast::channel(QUEUE_CAPACITY);
        let state = Arc::new(Mutex::new(State {
            pending: HashMap::new(),
            closed: None,
            events: Some(events),
            initial: Some(initial),
        }));
        let permits = Arc::new(Semaphore::new(max_requests));
        let stop = CancellationToken::new();
        let (outbound, outgoing) = mpsc::channel(QUEUE_CAPACITY);
        let (writer_finished, writer_done) = watch::channel(None);
        tokio::spawn(read_loop(
            reader,
            state.clone(),
            permits.clone(),
            stop.clone(),
        ));
        tokio::spawn(write_loop(
            writer,
            outgoing,
            state.clone(),
            permits.clone(),
            stop.clone(),
            maximum,
            writer_finished,
        ));
        Ok(Self {
            next_id: AtomicU64::new(1),
            outbound,
            state,
            permits,
            _close_on_drop: stop.clone().drop_guard(),
            stop,
            request_timeout,
            writer_done,
        })
    }
    /// Lag is explicit through `RecvError::Lagged`; consumers must refresh state.
    pub fn subscribe(&self) -> broadcast::Receiver<PeerEvent> {
        let mut state = self.state.lock().unwrap();
        if let Some(initial) = state.initial.take() {
            return initial;
        }
        if let Some(events) = &state.events {
            return events.subscribe();
        }
        let (sender, receiver) = broadcast::channel(1);
        drop(sender);
        receiver
    }
    /// Stops new work and waits for the writer's transport shutdown.
    pub async fn close(&self) -> Result<(), PeerError> {
        self.stop.cancel();
        let mut done = self.writer_done.clone();
        loop {
            if let Some(result) = done.borrow_and_update().clone() {
                return result.map_err(PeerError::ConnectionClosed);
            }
            done.changed()
                .await
                .map_err(|_| PeerError::ConnectionClosed("writer task stopped".into()))?;
        }
    }

    /// Returns the original response line with only the caller-owned top-level ID restored.
    pub fn request_raw<'a>(
        &'a self,
        line: &str,
    ) -> Request<impl Future<Output = Result<Reply<String>, PeerError>> + Send + use<'a>> {
        let prepared = self.prepare(line);
        Request {
            wire_id: prepared.as_ref().ok().map(|request| request.id),
            response: async move { self.exchange(prepared?).await },
        }
    }
    fn prepare(&self, line: &str) -> Result<PreparedRequest, PeerError> {
        let message = classify_message(line).map_err(invalid)?;
        if message.kind() != RpcMessageKind::Request {
            return Err(invalid("request must contain method and id"));
        }
        let original_id = message
            .raw_id()
            .ok_or_else(|| invalid("request ID is missing"))?
            .to_owned();
        let method = message
            .method()
            .ok_or_else(|| invalid("request method is missing"))?
            .to_owned();
        let id = allocate_id(&self.next_id)?;
        let id_text = id.to_string();
        let line = if original_id == id_text {
            line.to_owned()
        } else {
            rewrite_top_level_id(line, &id_text).map_err(invalid)?
        };
        Ok(PreparedRequest {
            original_id,
            method,
            id,
            line,
        })
    }
    async fn exchange(&self, prepared: PreparedRequest) -> Result<Reply<String>, PeerError> {
        let PreparedRequest {
            original_id,
            method,
            id,
            line,
        } = prepared;
        let deadline = tokio::time::Instant::now() + self.request_timeout;
        let _permit = match tokio::time::timeout_at(deadline, self.permits.acquire()).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => return Err(self.closed_error()),
            Err(_) => return Err(PeerError::RequestTimeout { method, id }),
        };
        let (tx, mut rx) = oneshot::channel();
        {
            let mut state = self.state.lock().unwrap();
            if let Some(reason) = &state.closed {
                let error = PeerError::ConnectionClosed(reason.clone());
                drop(state);
                return Err(error);
            }
            state.pending.insert(
                id,
                Pending {
                    original_id,
                    method: Arc::from(method.as_str()),
                    complete: tx,
                },
            );
        }
        let _cleanup = scopeguard::guard((&self.state, id), |(state, id)| {
            state.lock().unwrap().pending.remove(&id);
        });
        match tokio::time::timeout_at(deadline, async {
            self.enqueue(line).await?;
            (&mut rx).await.map_err(|_| self.closed_error())
        })
        .await
        {
            Ok(Ok(value)) => value,
            failure => {
                let error = match failure {
                    Ok(Err(error)) => error,
                    Err(_) => PeerError::RequestTimeout { method, id },
                    _ => unreachable!(),
                };
                let pending = self.state.lock().unwrap().pending.remove(&id);
                if let Some(pending) = pending {
                    let _ = pending.complete.send(Err(error));
                }
                rx.await.map_err(|_| self.closed_error())?
            }
        }
    }
    pub fn request<'a, P: Serialize, T: DeserializeOwned>(
        &'a self,
        method: &'a str,
        params: &P,
    ) -> Request<impl Future<Output = Result<Reply<T>, PeerError>> + Send + use<'a, P, T>> {
        let prepared = request_line(method, params).and_then(|line| self.prepare(&line));
        Request {
            wire_id: prepared.as_ref().ok().map(|request| request.id),
            response: async move {
                let reply = self.exchange(prepared?).await?;
                let object = raw_object(&reply.value).map_err(invalid)?;
                if let Some(error) = object.get("error") {
                    return Err(PeerError::Remote {
                        error: error.get().into(),
                        sequence: Some(reply.sequence),
                    });
                }
                let result = object
                    .get("result")
                    .ok_or_else(|| invalid("response has no result or error"))?;
                Ok(Reply {
                    sequence: reply.sequence,
                    value: serde_json::from_str(result.get()).map_err(|error| {
                        PeerError::InvalidResponse {
                            method: method.into(),
                            reason: error.to_string(),
                            raw: result.get().into(),
                            sequence: Some(reply.sequence),
                        }
                    })?,
                })
            },
        }
    }
    pub async fn send_raw(&self, line: impl Into<String>) -> Result<(), PeerError> {
        let line = line.into();
        if classify_message(&line).map_err(invalid)?.kind() == RpcMessageKind::Request {
            return Err(invalid("raw requests must go through request_raw"));
        }
        self.enqueue(line).await
    }
    pub async fn respond_raw(
        &self,
        id: &str,
        field: &'static str,
        payload: &str,
    ) -> Result<(), PeerError> {
        self.send_raw(&response_line(id, field, payload)?).await
    }
    async fn enqueue(&self, line: String) -> Result<(), PeerError> {
        if self.stop.is_cancelled() {
            return Err(self.closed_error());
        }
        let (written, completed) = oneshot::channel();
        self.outbound
            .send(Outbound { line, written })
            .await
            .map_err(|_| self.closed_error())?;
        tokio::select! {
            result = completed => result.map_err(|_| self.closed_error()),
            _ = self.stop.cancelled() => Err(self.closed_error()),
        }
    }
    fn closed_error(&self) -> PeerError {
        PeerError::ConnectionClosed(
            self.state
                .lock()
                .unwrap()
                .closed
                .clone()
                .unwrap_or_else(|| "peer is closed".into()),
        )
    }
}

fn allocate_id(next: &AtomicU64) -> Result<u64, PeerError> {
    next.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map_err(|_| PeerError::RequestIdExhausted)
}
pub fn request_line<P: Serialize>(method: &str, params: &P) -> Result<String, PeerError> {
    #[derive(Serialize)]
    struct Request<'a, P> {
        id: u64,
        method: &'a str,
        params: &'a P,
    }
    serde_json::to_string(&Request {
        id: 0,
        method,
        params,
    })
    .map_err(invalid)
}
pub fn response_line(id: &str, field: &'static str, payload: &str) -> Result<String, PeerError> {
    if !matches!(field, "result" | "error") {
        return Err(invalid("response field must be result or error"));
    }
    let id: &RawValue = serde_json::from_str(id).map_err(invalid)?;
    let payload: &RawValue = serde_json::from_str(payload).map_err(invalid)?;
    #[derive(Serialize)]
    struct ResultResponse<'a> {
        id: &'a RawValue,
        result: &'a RawValue,
    }
    #[derive(Serialize)]
    struct ErrorResponse<'a> {
        id: &'a RawValue,
        error: &'a RawValue,
    }
    if field == "result" {
        serde_json::to_string(&ResultResponse {
            id,
            result: payload,
        })
        .map_err(invalid)
    } else {
        serde_json::to_string(&ErrorResponse { id, error: payload }).map_err(invalid)
    }
}
fn invalid(error: impl std::fmt::Display) -> PeerError {
    PeerError::InvalidMessage(error.to_string())
}

async fn read_loop<R: AsyncRead + Unpin>(
    mut reader: JsonlReader<R>,
    state: Arc<Mutex<State>>,
    permits: Arc<Semaphore>,
    stop: CancellationToken,
) {
    let mut sequence = 0u64;
    let reason = loop {
        let line = tokio::select! {
            _ = stop.cancelled() => break "peer closed".into(),
            result = reader.read_line() => match result { Ok(Some(line)) => line, Ok(None) => break "JSONL stream reached EOF".into(), Err(error) => break error.to_string() }
        };
        let message = match classify_message(&line) {
            Ok(message) => message,
            Err(error) => break error.to_string(),
        };
        sequence += 1;
        if message.kind() == RpcMessageKind::Response {
            let id = message
                .raw_id()
                .and_then(|id| serde_json::from_str::<u64>(id).ok());
            let pending = {
                let mut state = state.lock().unwrap();
                let pending = id.and_then(|id| state.pending.remove(&id));
                if let Some(events) = &state.events {
                    let _ = events.send(PeerEvent::Response {
                        sequence,
                        request_id: id,
                        method: pending.as_ref().map(|pending| pending.method.clone()),
                    });
                }
                pending
            };
            if let Some(pending) = pending {
                let response = if message.raw_id() == Some(pending.original_id.as_str()) {
                    Ok(line)
                } else {
                    rewrite_top_level_id(&line, &pending.original_id).map_err(invalid)
                };
                let _ = pending
                    .complete
                    .send(response.map(|value| Reply { sequence, value }));
            }
        } else {
            let state = state.lock().unwrap();
            if let Some(events) = &state.events {
                let _ = events.send(PeerEvent::Message(Reply {
                    sequence,
                    value: Arc::from(line.as_str()),
                }));
            }
        }
    };
    terminate(&state, &permits, &stop, reason);
}
async fn write_loop<W: AsyncWrite + Unpin>(
    writer: W,
    mut outgoing: mpsc::Receiver<Outbound>,
    state: Arc<Mutex<State>>,
    permits: Arc<Semaphore>,
    stop: CancellationToken,
    maximum: usize,
    finished: watch::Sender<Option<Result<(), String>>>,
) {
    let mut writer = JsonlWriter::with_max_message_bytes(writer, maximum);
    let reason = loop {
        let line = tokio::select! { _ = stop.cancelled() => break "peer closed".into(), line = outgoing.recv() => match line { Some(line) => line, None => break "JSONL writer stopped".into() } };
        let result = tokio::select! { _ = stop.cancelled() => break "peer closed".into(), result = writer.write_line(&line.line) => result };
        if let Err(error) = result {
            break error.to_string();
        }
        let _ = line.written.send(());
    };
    let result = match tokio::time::timeout(Duration::from_secs(3), writer.shutdown()).await {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(_) => Err("writer shutdown timed out".into()),
    };
    finished.send_replace(Some(result));
    terminate(&state, &permits, &stop, reason);
}
fn terminate(state: &Mutex<State>, permits: &Semaphore, stop: &CancellationToken, reason: String) {
    let pending = {
        let mut state = state.lock().unwrap();
        if state.closed.is_some() {
            return;
        }
        state.closed = Some(reason.clone());
        if let Some(events) = state.events.take() {
            let _ = events.send(PeerEvent::Closed(reason.clone()));
        }
        std::mem::take(&mut state.pending)
    };
    permits.close();
    stop.cancel();
    for pending in pending.into_values() {
        let _ = pending
            .complete
            .send(Err(PeerError::ConnectionClosed(reason.clone())));
    }
}

#[cfg(test)]
mod tests {
    use super::PeerError as Error;
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
            Arc::new(
                RpcPeer::open(
                    JsonlReader::new(client_reader),
                    client_writer,
                    Duration::from_secs(1),
                    1024,
                )
                .unwrap(),
            ),
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

        let first: Value = serde_json::from_str(&first.await.unwrap().unwrap().value).unwrap();
        let second: Value = serde_json::from_str(&second.await.unwrap().unwrap().value).unwrap();
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

        let PeerEvent::Message(first) = events.recv().await.unwrap() else {
            panic!("closed")
        };
        let PeerEvent::Message(second) = events.recv().await.unwrap() else {
            panic!("closed")
        };
        assert_eq!(&*first.value, notification);
        assert_eq!(&*second.value, request);
        assert_eq!(second.sequence, first.sequence + 1);
    }

    #[tokio::test]
    async fn response_and_following_delta_share_wire_order() {
        let (peer, server_reader, mut writer) = make_peer();
        let mut events = peer.subscribe();
        let server = async {
            let mut reader = BufReader::new(server_reader);
            let request: Value = serde_json::from_str(&read_line(&mut reader).await).unwrap();
            writer.write_all(format!("{{\"id\":{},\"result\":{{\"text\":\"base\"}}}}\n{{\"method\":\"delta\",\"params\":{{\"text\":\"next\"}}}}\n", request["id"]).as_bytes()).await.unwrap();
            writer
        };
        let params = json!({});
        let (reply, _writer) = tokio::join!(peer.request::<_, Value>("read", &params), server);
        let reply = reply.unwrap();
        assert_eq!(reply.value["text"], "base");
        let PeerEvent::Response { sequence, method, .. } = events.recv().await.unwrap() else {
            panic!("expected response")
        };
        let PeerEvent::Message(delta) = events.recv().await.unwrap() else {
            panic!("closed")
        };
        assert_eq!(sequence, reply.sequence);
        assert_eq!(method.as_deref(), Some("read"));
        assert_eq!(delta.sequence, reply.sequence + 1);
        assert_eq!(
            serde_json::from_str::<Value>(&delta.value).unwrap()["method"],
            "delta"
        );
    }

    #[tokio::test]
    async fn response_completion_means_written_before_close() {
        use std::{future::Future, task::Poll};
        let (peer, server_reader, _server_writer) = make_peer();
        let response = r#"{"id":"approval","result":{"decision":"decline"}}"#;
        let mut send = std::pin::pin!(peer.send_raw(response));
        std::future::poll_fn(|cx| {
            assert!(
                send.as_mut().poll(cx).is_pending(),
                "must wait for the writer"
            );
            Poll::Ready(())
        })
        .await;
        send.await.unwrap();
        peer.close().await.unwrap();
        let mut lines = BufReader::new(server_reader);
        assert_eq!(read_line(&mut lines).await, response);
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
        let response: Value = serde_json::from_str(&request.await.unwrap().unwrap().value).unwrap();
        assert_eq!(response["id"], "caller");
        assert_eq!(response["result"]["ok"], true);
    }

    #[tokio::test]
    async fn timeout_removes_pending_request() {
        let (client_io, server_io) = duplex(32 * 1024);
        let (client_reader, client_writer) = tokio::io::split(client_io);
        let (server_reader, _server_writer) = tokio::io::split(server_io);
        let peer = Arc::new(
            RpcPeer::open(
                JsonlReader::new(client_reader),
                client_writer,
                Duration::from_millis(10),
                1024,
            )
            .unwrap(),
        );
        let mut lines = BufReader::new(server_reader);
        let error = peer
            .request_raw(r#"{"id":"timeout","method":"blocked","params":{}}"#)
            .await
            .unwrap_err();
        let _ = read_line(&mut lines).await;
        assert!(matches!(error, Error::RequestTimeout { method, .. } if method == "blocked"));
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
        let response: Value = serde_json::from_str(&next.await.unwrap().unwrap().value).unwrap();
        assert_eq!(response["id"], "next");
        assert_eq!(response["result"]["ok"], true);
    }
}

#[cfg(test)]
mod envelope_tests {
    use super::*;
    #[test]
    fn typed_envelopes_keep_raw_params_and_response_payloads() {
        let params = r#"{"future": {"id": "nested"}, "text": "hello"}"#;
        let raw: &RawValue = serde_json::from_str(params).unwrap();
        let method = "custom/\"日本語\\method";
        let line = request_line(method, &raw).unwrap();
        let object = raw_object(&line).unwrap();
        assert_eq!(object["params"].get(), params);
        assert_eq!(
            serde_json::from_str::<String>(object["method"].get()).unwrap(),
            method
        );
        let error = r#"{ "code": -1, "message": "失敗", "data": [null,{"id":7}] }"#;
        let response = response_line(r#""request-7""#, "error", error).unwrap();
        let object = raw_object(&response).unwrap();
        assert_eq!(object["id"].get(), r#""request-7""#);
        assert_eq!(object["error"].get(), error);
        assert!(response_line("7 8", "result", "null").is_err());
        assert!(response_line("7", "error", "{").is_err());
        assert!(response_line("7", "params", "{}").is_err());
    }
    #[test]
    fn request_ids_stop_at_exhaustion_without_wrapping() {
        let next = AtomicU64::new(u64::MAX - 1);
        assert_eq!(allocate_id(&next).unwrap(), u64::MAX - 1);
        assert!(matches!(
            allocate_id(&next),
            Err(PeerError::RequestIdExhausted)
        ));
        assert_eq!(next.load(Ordering::Relaxed), u64::MAX);
    }
}
