//! One lossless, bidirectional JSONL peer for local sockets, relay streams and child processes.
use host_protocol::{
    JsonlReader, JsonlWriter, RpcMessageKind, classify_message, raw_object, rewrite_top_level_id,
};
use serde::Serialize;
use serde_json::{Value, value::RawValue};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{Semaphore, broadcast, mpsc, oneshot},
};
use tokio_util::sync::{CancellationToken, DropGuard};

const QUEUE_CAPACITY: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    #[error("invalid JSONL message: {0}")]
    InvalidMessage(String),
    #[error("connection closed: {0}")]
    ConnectionClosed(String),
    #[error("request {method} timed out")]
    RequestTimeout { method: String, id: u64 },
    #[error("request ID space exhausted")]
    RequestIdExhausted,
    #[error("remote RPC error: {error}")]
    Remote { error: String },
}
pub enum PeerEvent {
    Message(String),
    Closed(String),
}
pub enum EventDelivery {
    Unified,
    SplitRequests,
    /// Runs on the reader, before the next frame is consumed.
    Callback(Arc<dyn Fn(PeerEvent) + Send + Sync>),
}
struct EventChannel {
    sender: Option<broadcast::Sender<String>>,
    initial: Option<broadcast::Receiver<String>>,
}
impl EventChannel {
    fn new() -> Self {
        let (sender, initial) = broadcast::channel(QUEUE_CAPACITY);
        Self {
            sender: Some(sender),
            initial: Some(initial),
        }
    }
}
struct PreparedRequest {
    original_id: String,
    method: String,
    id: u64,
    line: String,
}
struct Pending {
    original_id: String,
    complete: Box<dyn FnOnce(Result<String, PeerError>) + Send>,
}
struct State {
    pending: HashMap<u64, Pending>,
    closed: Option<String>,
    events: Option<EventChannel>,
    requests: Option<EventChannel>,
}
pub struct RpcPeer {
    next_id: AtomicU64,
    outbound: mpsc::Sender<String>,
    state: Arc<Mutex<State>>,
    permits: Arc<Semaphore>,
    stop: CancellationToken,
    _close_on_drop: DropGuard,
    request_timeout: Duration,
}

impl RpcPeer {
    pub fn open<R, W>(
        reader: JsonlReader<R>,
        writer: W,
        request_timeout: Duration,
        max_requests: usize,
        delivery: EventDelivery,
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
        let events = (!matches!(delivery, EventDelivery::Callback(_))).then(EventChannel::new);
        let requests = matches!(delivery, EventDelivery::SplitRequests).then(EventChannel::new);
        let callback = match delivery {
            EventDelivery::Callback(callback) => Some(callback),
            _ => None,
        };
        let state = Arc::new(Mutex::new(State {
            pending: HashMap::new(),
            closed: None,
            events,
            requests,
        }));
        let permits = Arc::new(Semaphore::new(max_requests));
        let stop = CancellationToken::new();
        let (outbound, outgoing) = mpsc::channel(QUEUE_CAPACITY);
        tokio::spawn(read_loop(
            reader,
            state.clone(),
            permits.clone(),
            stop.clone(),
            callback.clone(),
        ));
        tokio::spawn(write_loop(
            writer,
            outgoing,
            state.clone(),
            permits.clone(),
            stop.clone(),
            maximum,
            callback,
        ));
        Ok(Self {
            next_id: AtomicU64::new(1),
            outbound,
            state,
            permits,
            _close_on_drop: stop.clone().drop_guard(),
            stop,
            request_timeout,
        })
    }
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        subscribe(&mut self.state.lock().unwrap().events)
    }
    pub fn subscribe_server_requests(&self) -> broadcast::Receiver<String> {
        subscribe(&mut self.state.lock().unwrap().requests)
    }
    pub fn close(&self) {
        self.stop.cancel();
    }

    /// Returns the original response line with only the caller-owned top-level ID restored.
    pub async fn request_raw(&self, line: &str) -> Result<String, PeerError> {
        self.exchange(self.prepare(line), self.request_timeout, |result| result)
            .await?
    }
    /// A completion and the following notification execute in wire order, even on multithreaded runtimes.
    pub async fn request_callback(
        &self,
        line: &str,
        timeout: Duration,
        complete: impl FnOnce(Result<String, PeerError>) + Send + 'static,
    ) {
        let _ = self.exchange(self.prepare(line), timeout, complete).await;
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
    /// Exposes the wire ID for servers that later emit a request-resolved notification.
    /// Reservation does no I/O; dropping the future also drops any pending response.
    pub fn request_identified<'a, P: Serialize>(
        &'a self,
        method: &str,
        params: &P,
    ) -> Result<
        (
            u64,
            impl std::future::Future<Output = Result<String, PeerError>> + use<'a, P>,
        ),
        PeerError,
    > {
        let prepared = self.prepare(&request_line(method, params)?)?;
        let id = prepared.id;
        Ok((id, async move {
            self.exchange(Ok(prepared), self.request_timeout, |result| result)
                .await?
        }))
    }
    async fn exchange<T: Send + 'static>(
        &self,
        prepared: Result<PreparedRequest, PeerError>,
        timeout: Duration,
        map: impl FnOnce(Result<String, PeerError>) -> T + Send + 'static,
    ) -> Result<T, PeerError> {
        let PreparedRequest {
            original_id,
            method,
            id,
            line,
        } = match prepared {
            Ok(value) => value,
            Err(error) => return Ok(map(Err(error))),
        };
        let deadline = tokio::time::Instant::now() + timeout;
        let _permit = match tokio::time::timeout_at(deadline, self.permits.acquire()).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => return Ok(map(Err(self.closed_error()))),
            Err(_) => return Ok(map(Err(PeerError::RequestTimeout { method, id }))),
        };
        let (tx, mut rx) = oneshot::channel();
        {
            let mut state = self.state.lock().unwrap();
            if let Some(reason) = &state.closed {
                let error = PeerError::ConnectionClosed(reason.clone());
                drop(state);
                return Ok(map(Err(error)));
            }
            state.pending.insert(
                id,
                Pending {
                    original_id,
                    complete: Box::new(move |result| {
                        let _ = tx.send(map(result));
                    }),
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
            Ok(Ok(value)) => Ok(value),
            failure => {
                let error = match failure {
                    Ok(Err(error)) => error,
                    Err(_) => PeerError::RequestTimeout { method, id },
                    _ => unreachable!(),
                };
                let pending = self.state.lock().unwrap().pending.remove(&id);
                if let Some(pending) = pending {
                    (pending.complete)(Err(error));
                }
                rx.await.map_err(|_| self.closed_error())
            }
        }
    }
    pub async fn request<P: Serialize>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<Value, PeerError> {
        let line = request_line(method, params)?;
        response_value(&self.request_raw(&line).await?)
    }
    pub async fn request_params_raw(&self, method: &str, params: &str) -> Result<Value, PeerError> {
        let params: &RawValue = serde_json::from_str(params).map_err(invalid)?;
        self.request(method, &params).await
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
        self.outbound
            .send(line)
            .await
            .map_err(|_| self.closed_error())
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

fn subscribe(channel: &mut Option<EventChannel>) -> broadcast::Receiver<String> {
    if let Some(channel) = channel {
        if let Some(initial) = channel.initial.take() {
            return initial;
        }
        if let Some(sender) = &channel.sender {
            return sender.subscribe();
        }
    }
    let (sender, receiver) = broadcast::channel(1);
    drop(sender);
    receiver
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
pub fn response_value(line: &str) -> Result<Value, PeerError> {
    let object = raw_object(line).map_err(invalid)?;
    if let Some(error) = object.get("error") {
        return Err(PeerError::Remote {
            error: error.get().into(),
        });
    }
    let result = object
        .get("result")
        .ok_or_else(|| invalid("response has no result or error"))?;
    serde_json::from_str(result.get()).map_err(invalid)
}
fn invalid(error: impl std::fmt::Display) -> PeerError {
    PeerError::InvalidMessage(error.to_string())
}

async fn read_loop<R: AsyncRead + Unpin>(
    mut reader: JsonlReader<R>,
    state: Arc<Mutex<State>>,
    permits: Arc<Semaphore>,
    stop: CancellationToken,
    callback: Option<Arc<dyn Fn(PeerEvent) + Send + Sync>>,
) {
    let reason = loop {
        let line = tokio::select! {
            _ = stop.cancelled() => break "peer closed".into(),
            result = reader.read_line() => match result { Ok(Some(line)) => line, Ok(None) => break "JSONL stream reached EOF".into(), Err(error) => break error.to_string() }
        };
        let message = match classify_message(&line) {
            Ok(message) => message,
            Err(error) => break error.to_string(),
        };
        match message.kind() {
            RpcMessageKind::Response => {
                let Some(id) = message
                    .raw_id()
                    .and_then(|id| serde_json::from_str::<u64>(id).ok())
                else {
                    continue;
                };
                let pending = state.lock().unwrap().pending.remove(&id);
                if let Some(pending) = pending {
                    let response = if message.raw_id() == Some(pending.original_id.as_str()) {
                        Ok(line)
                    } else {
                        rewrite_top_level_id(&line, &pending.original_id).map_err(invalid)
                    };
                    (pending.complete)(response);
                }
            }
            kind => {
                if let Some(callback) = &callback {
                    callback(PeerEvent::Message(line));
                } else {
                    let state = state.lock().unwrap();
                    let channel = if kind == RpcMessageKind::Request {
                        state.requests.as_ref().or(state.events.as_ref())
                    } else {
                        state.events.as_ref()
                    };
                    if let Some(sender) = channel.and_then(|channel| channel.sender.as_ref()) {
                        let _ = sender.send(line);
                    }
                }
            }
        }
    };
    terminate(&state, &permits, &stop, &callback, reason);
}
async fn write_loop<W: AsyncWrite + Unpin>(
    writer: W,
    mut outgoing: mpsc::Receiver<String>,
    state: Arc<Mutex<State>>,
    permits: Arc<Semaphore>,
    stop: CancellationToken,
    maximum: usize,
    callback: Option<Arc<dyn Fn(PeerEvent) + Send + Sync>>,
) {
    let mut writer = JsonlWriter::with_max_message_bytes(writer, maximum);
    let reason = loop {
        let line = tokio::select! { _ = stop.cancelled() => break "peer closed".into(), line = outgoing.recv() => match line { Some(line) => line, None => break "JSONL writer stopped".into() } };
        let result = tokio::select! { _ = stop.cancelled() => break "peer closed".into(), result = writer.write_line(&line) => result };
        if let Err(error) = result {
            break error.to_string();
        }
    };
    terminate(&state, &permits, &stop, &callback, reason);
}
fn terminate(
    state: &Mutex<State>,
    permits: &Semaphore,
    stop: &CancellationToken,
    callback: &Option<Arc<dyn Fn(PeerEvent) + Send + Sync>>,
    reason: String,
) {
    let pending = {
        let mut state = state.lock().unwrap();
        if state.closed.is_some() {
            return;
        }
        state.closed = Some(reason.clone());
        if let Some(channel) = &mut state.events {
            channel.sender.take();
        }
        if let Some(channel) = &mut state.requests {
            channel.sender.take();
        }
        std::mem::take(&mut state.pending)
    };
    permits.close();
    stop.cancel();
    for pending in pending.into_values() {
        (pending.complete)(Err(PeerError::ConnectionClosed(reason.clone())));
    }
    if let Some(callback) = callback {
        callback(PeerEvent::Closed(reason));
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
                    EventDelivery::Unified,
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
    async fn identified_requests_expose_wire_id_and_preserve_error_envelopes() {
        let (peer, server_reader, mut writer) = make_peer();
        let (id, response) = peer
            .request_identified("account/refresh", &json!({}))
            .unwrap();
        let server = async {
            let mut reader = BufReader::new(server_reader);
            let request: Value = serde_json::from_str(&read_line(&mut reader).await).unwrap();
            assert_eq!(request["id"], id);
            writer.write_all(format!("{{\"id\":{id},\"error\":{{\"code\":-1,\"message\":\"denied\"}},\"future\":true}}\n").as_bytes()).await.unwrap();
        };
        let (response, ()) = tokio::join!(response, server);
        let response: Value = serde_json::from_str(&response.unwrap()).unwrap();
        assert_eq!(response["error"]["message"], "denied");
        assert_eq!(response["future"], true);
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
        let peer = Arc::new(
            RpcPeer::open(
                JsonlReader::new(client_reader),
                client_writer,
                Duration::from_millis(10),
                1024,
                EventDelivery::Unified,
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
    async fn callback_uses_the_callers_deadline() {
        let (peer, _server_reader, _server_writer) = make_peer();
        let (sent, received) = tokio::sync::oneshot::channel();
        tokio::time::timeout(
            Duration::from_secs(1),
            peer.request_callback(
                r#"{"id":"callback","method":"blocked","params":{}}"#,
                Duration::ZERO,
                move |reply| {
                    let _ = sent.send(reply);
                },
            ),
        )
        .await
        .unwrap();
        assert!(matches!(
            received.await.unwrap(),
            Err(PeerError::RequestTimeout { .. })
        ));
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
