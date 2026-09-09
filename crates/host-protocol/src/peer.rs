//! Transport-independent JSONL request correlation and ordered event delivery.
use crate::{JsonlReader, JsonlWriter, RpcMessageKind, classify_message, rewrite_top_level_id};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{OwnedSemaphorePermit, Semaphore, broadcast, mpsc, oneshot, watch},
    time::timeout,
};

/// Shared by Host admission and its clients. Responses never consume a slot.
pub const HOST_REQUEST_LIMIT: usize = 8;
const OUTBOUND_CAPACITY: usize = 128;

#[derive(Debug, Clone, thiserror::Error)]
pub enum RpcPeerError {
    #[error("invalid RPC message: {0}")]
    InvalidMessage(String),
    #[error("RPC connection closed: {0}")]
    ConnectionClosed(String),
    #[error("RPC request {method} timed out")]
    RequestTimeout { method: String },
    #[error("request ID space exhausted")]
    RequestIdExhausted,
}

pub enum RpcEvent {
    Message(String),
    Closed(String),
}

type Completion = Box<dyn FnOnce(Result<String, RpcPeerError>) + Send>;
struct Pending {
    original_id: String,
    completion: Option<Completion>,
    // Cancellation stops delivery, not remote execution. Keep admission until
    // its response or disconnect, so another request cannot exceed the Host cap.
    _permit: OwnedSemaphorePermit,
}
struct State {
    pending: HashMap<u64, Pending>,
    terminal: Option<String>,
}
struct Shared {
    state: Mutex<State>,
    permits: Arc<Semaphore>,
    stopped: watch::Sender<bool>,
    events: Box<dyn Fn(RpcEvent) + Send + Sync>,
}

pub struct RpcPeer {
    shared: Arc<Shared>,
    next_id: AtomicU64,
    outbound: mpsc::Sender<String>,
}

impl RpcPeer {
    pub fn open<R, W>(
        reader: R,
        writer: W,
        max_message_bytes: usize,
        request_limit: usize,
        events: impl Fn(RpcEvent) + Send + Sync + 'static,
    ) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        assert!(max_message_bytes > 0 && request_limit > 0);
        let (outbound, mut outgoing) = mpsc::channel::<String>(OUTBOUND_CAPACITY);
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                pending: HashMap::new(),
                terminal: None,
            }),
            permits: Arc::new(Semaphore::new(request_limit)),
            stopped: watch::channel(false).0,
            events: Box::new(events),
        });
        let write_state = shared.clone();
        let mut write_stop = shared.stopped.subscribe();
        tokio::spawn(async move {
            let mut writer = JsonlWriter::with_max_message_bytes(writer, max_message_bytes);
            let reason = loop {
                if *write_stop.borrow() {
                    return;
                }
                tokio::select! {
                    biased;
                    _ = write_stop.changed() => return,
                    result = async {
                        let line = outgoing.recv().await.ok_or_else(|| "writer queue closed".to_owned())?;
                        writer.write_line(&line).await.map_err(|error| error.to_string())
                    } => if let Err(reason) = result { break reason; }
                }
            };
            terminate(&write_state, reason);
        });
        let read_state = shared.clone();
        let mut read_stop = shared.stopped.subscribe();
        tokio::spawn(async move {
            let mut reader = JsonlReader::with_max_message_bytes(reader, max_message_bytes);
            let reason = loop {
                if *read_stop.borrow() {
                    return;
                }
                let line = tokio::select! {
                    biased;
                    _ = read_stop.changed() => return,
                    result = reader.read_line() => match result {
                        Ok(Some(line)) => line,
                        Ok(None) => break "input reached EOF".to_owned(),
                        Err(error) => break error.to_string(),
                    }
                };
                let message = match classify_message(&line) {
                    Ok(message) => message,
                    Err(error) => break error.to_string(),
                };
                match message.kind() {
                    RpcMessageKind::Notification | RpcMessageKind::Request => {
                        (read_state.events)(RpcEvent::Message(line))
                    }
                    RpcMessageKind::Response => {
                        let Some(raw_id) = message.raw_id() else {
                            continue;
                        };
                        let Ok(id) = serde_json::from_str::<u64>(raw_id) else {
                            continue;
                        };
                        let pending = read_state.state.lock().unwrap().pending.remove(&id);
                        if let Some(mut pending) = pending {
                            if let Some(done) = pending.completion.take() {
                                let result = if pending.original_id == raw_id {
                                    Ok(line)
                                } else {
                                    rewrite_top_level_id(&line, &pending.original_id).map_err(
                                        |error| RpcPeerError::InvalidMessage(error.to_string()),
                                    )
                                };
                                // This callback completes before the next wire event, including
                                // when the consumer enqueues UI work on another executor.
                                done(result);
                            }
                        }
                    }
                }
            };
            terminate(&read_state, reason);
        });
        Self {
            shared,
            next_id: AtomicU64::new(1),
            outbound,
        }
    }

    pub async fn request_raw(
        &self,
        line: &str,
        deadline: Duration,
    ) -> Result<String, RpcPeerError> {
        let (tx, rx) = oneshot::channel();
        self.request_with(line, deadline, move |result| {
            let _ = tx.send(result);
        })
        .await;
        rx.await
            .map_err(|_| RpcPeerError::ConnectionClosed("completion stopped".into()))?
    }

    /// Completion runs on the reader for a reply, or on this future for local
    /// errors. Dropping this future cancels delivery without replaying the request.
    pub async fn request_with(
        &self,
        line: &str,
        deadline: Duration,
        done: impl FnOnce(Result<String, RpcPeerError>) + Send + 'static,
    ) {
        let mut done: Option<Completion> = Some(Box::new(done));
        let mut registration = Registration {
            shared: self.shared.clone(),
            id: None,
        };
        let mut method = String::new();
        let result = async {
            let message =
                classify_message(line).map_err(|e| RpcPeerError::InvalidMessage(e.to_string()))?;
            if message.kind() != RpcMessageKind::Request {
                return Err(RpcPeerError::InvalidMessage(
                    "request needs method and id".into(),
                ));
            }
            method = message.method().unwrap().to_owned();
            let original_id = message.raw_id().unwrap().to_owned();
            let exchange = async {
                let permit = self
                    .shared
                    .permits
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| self.closed_error())?;
                let outbound = self
                    .outbound
                    .reserve()
                    .await
                    .map_err(|_| self.closed_error())?;
                let id = self
                    .next_id
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                    .map_err(|_| RpcPeerError::RequestIdExhausted)?;
                let upstream_id = id.to_string();
                let line = if original_id == upstream_id {
                    line.to_owned()
                } else {
                    rewrite_top_level_id(line, &upstream_id)
                        .map_err(|e| RpcPeerError::InvalidMessage(e.to_string()))?
                };
                let (completed, completion) = oneshot::channel();
                {
                    let mut state = self.shared.state.lock().unwrap();
                    if let Some(reason) = &state.terminal {
                        return Err(RpcPeerError::ConnectionClosed(reason.clone()));
                    }
                    let callback = done.take().unwrap();
                    state.pending.insert(
                        id,
                        Pending {
                            original_id,
                            completion: Some(Box::new(move |result| {
                                callback(result);
                                let _ = completed.send(());
                            })),
                            _permit: permit,
                        },
                    );
                    registration.id = Some(id);
                    outbound.send(line);
                }
                completion.await.map_err(|_| self.closed_error())
            };
            timeout(deadline, exchange)
                .await
                .map_err(|_| RpcPeerError::RequestTimeout {
                    method: method.clone(),
                })?
        }
        .await;
        if let Err(error) = result {
            if let Some(done) = done.take().or_else(|| registration.take_completion()) {
                done(Err(error));
            }
        }
    }

    pub async fn send_raw(&self, line: &str) -> Result<(), RpcPeerError> {
        let message =
            classify_message(line).map_err(|e| RpcPeerError::InvalidMessage(e.to_string()))?;
        if message.kind() == RpcMessageKind::Request {
            return Err(RpcPeerError::InvalidMessage(
                "requests must use request_raw".into(),
            ));
        }
        if self.shared.state.lock().unwrap().terminal.is_some() {
            return Err(self.closed_error());
        }
        self.outbound
            .send(line.to_owned())
            .await
            .map_err(|_| self.closed_error())
    }

    pub fn close(&self) {
        terminate(&self.shared, "connection closed".into());
    }

    fn closed_error(&self) -> RpcPeerError {
        RpcPeerError::ConnectionClosed(
            self.shared
                .state
                .lock()
                .unwrap()
                .terminal
                .clone()
                .unwrap_or_else(|| "transport stopped".into()),
        )
    }
}
impl Drop for RpcPeer {
    fn drop(&mut self) {
        self.close();
    }
}

struct Registration {
    shared: Arc<Shared>,
    id: Option<u64>,
}
impl Registration {
    fn take_completion(&self) -> Option<Completion> {
        self.shared
            .state
            .lock()
            .unwrap()
            .pending
            .get_mut(&self.id?)
            .and_then(|pending| pending.completion.take())
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.take_completion();
    }
}

fn terminate(shared: &Shared, reason: String) {
    let pending = {
        let mut state = shared.state.lock().unwrap();
        if state.terminal.is_some() {
            return;
        }
        state.terminal = Some(reason.clone());
        std::mem::take(&mut state.pending)
    };
    shared.permits.close();
    shared.stopped.send_replace(true);
    for mut pending in pending.into_values() {
        if let Some(done) = pending.completion.take() {
            done(Err(RpcPeerError::ConnectionClosed(reason.clone())));
        }
    }
    (shared.events)(RpcEvent::Closed(reason));
}

/// One bounded event stream retains early events until the first subscriber.
/// Overflow is reported as Lagged; clients must reconnect/resynchronize.
#[derive(Clone)]
pub struct RpcEventQueue(Arc<Mutex<EventQueue>>);
struct EventQueue {
    sender: Option<broadcast::Sender<String>>,
    initial: Option<broadcast::Receiver<String>>,
}
impl RpcEventQueue {
    pub fn new(capacity: usize) -> Self {
        let (sender, initial) = broadcast::channel(capacity);
        Self(Arc::new(Mutex::new(EventQueue {
            sender: Some(sender),
            initial: Some(initial),
        })))
    }
    pub fn deliver(&self, event: RpcEvent) {
        let mut queue = self.0.lock().unwrap();
        match event {
            RpcEvent::Message(line) => {
                if let Some(sender) = &queue.sender {
                    let _ = sender.send(line);
                }
            }
            RpcEvent::Closed(_) => {
                queue.sender.take();
            }
        }
    }
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        let mut queue = self.0.lock().unwrap();
        if let Some(initial) = queue.initial.take() {
            return initial;
        }
        if let Some(sender) = &queue.sender {
            return sender.subscribe();
        }
        broadcast::channel(1).1
    }
}

/// Native adapters share a fixed worker pool; request count never creates OS threads.
pub fn rpc_runtime() -> Result<&'static tokio::runtime::Runtime, &'static str> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use tokio::io::{DuplexStream, ReadHalf, WriteHalf};

    fn pair(
        limit: usize,
        events: impl Fn(RpcEvent) + Send + Sync + 'static,
    ) -> (
        Arc<RpcPeer>,
        JsonlReader<ReadHalf<DuplexStream>>,
        JsonlWriter<WriteHalf<DuplexStream>>,
    ) {
        let (client, server) = tokio::io::duplex(8192);
        let (read, write) = tokio::io::split(client);
        let (server_read, server_write) = tokio::io::split(server);
        (
            Arc::new(RpcPeer::open(read, write, 8192, limit, events)),
            JsonlReader::new(server_read),
            JsonlWriter::new(server_write),
        )
    }
    async fn receive(reader: &mut JsonlReader<ReadHalf<DuplexStream>>) -> Value {
        serde_json::from_str(&reader.read_line().await.unwrap().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn cancelled_request_keeps_admission_until_reply_but_does_not_block_approval_response() {
        timeout(Duration::from_secs(2), async {
            let (peer, mut reader, mut writer) = pair(1, |_| {});
            let first = {
                let peer = peer.clone();
                tokio::spawn(async move {
                    peer.request_raw(r#"{"id":"first","method":"first"}"#, Duration::from_secs(1))
                        .await
                })
            };
            let first_wire = receive(&mut reader).await;
            first.abort();
            assert!(first.await.unwrap_err().is_cancelled());
            let mut second = Box::pin(peer.request_raw(
                r#"{"id":"second","method":"second"}"#,
                Duration::from_secs(1),
            ));
            assert!(futures_util::poll!(second.as_mut()).is_pending());
            peer.send_raw(r#"{"id":"approval","result":{"decision":"decline"}}"#)
                .await
                .unwrap();
            assert_eq!(receive(&mut reader).await["id"], "approval");
            writer
                .write_line(&json!({"id":first_wire["id"],"result":{}}).to_string())
                .await
                .unwrap();
            let (reply, ()) = tokio::join!(second, async {
                let next = receive(&mut reader).await;
                assert_eq!(next["method"], "second");
                writer
                    .write_line(&json!({"id":next["id"],"result":{"ok":true}}).to_string())
                    .await
                    .unwrap();
            });
            let reply: Value = serde_json::from_str(&reply.unwrap()).unwrap();
            assert_eq!(reply["id"], "second");
            assert_eq!(reply["result"]["ok"], true);
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn completion_is_delivered_before_the_next_wire_event() {
        timeout(Duration::from_secs(2), async {
            let (delivered, mut deliveries) = mpsc::unbounded_channel();
            let event_delivery = delivered.clone();
            let (peer, mut reader, mut writer) = pair(1, move |event| {
                if let RpcEvent::Message(_) = event {
                    event_delivery.send("delta").unwrap();
                }
            });
            let request = tokio::spawn(async move {
                peer.request_with(
                    r#"{"id":"snapshot","method":"thread/read"}"#,
                    Duration::from_secs(1),
                    move |reply| {
                        assert!(reply.is_ok());
                        delivered.send("snapshot").unwrap();
                    },
                )
                .await;
            });
            let sent = receive(&mut reader).await;
            writer
                .write_line(&json!({"id":sent["id"],"result":{}}).to_string())
                .await
                .unwrap();
            writer
                .write_line(r#"{"method":"item/agentMessage/delta","params":{"delta":"next"}}"#)
                .await
                .unwrap();
            assert_eq!(deliveries.recv().await.unwrap(), "snapshot");
            assert_eq!(deliveries.recv().await.unwrap(), "delta");
            request.await.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn disconnect_fails_waiting_and_active_requests_and_closes_events() {
        timeout(Duration::from_secs(2), async {
            let events = RpcEventQueue::new(8);
            let mut incoming = events.subscribe();
            let (peer, mut reader, writer) = pair(1, move |event| events.deliver(event));
            let first = {
                let peer = peer.clone();
                tokio::spawn(async move {
                    peer.request_raw(r#"{"id":1,"method":"active"}"#, Duration::from_secs(1))
                        .await
                })
            };
            let _ = receive(&mut reader).await;
            let mut waiting = Box::pin(
                peer.request_raw(r#"{"id":2,"method":"waiting"}"#, Duration::from_secs(1)),
            );
            assert!(futures_util::poll!(waiting.as_mut()).is_pending());
            drop(writer);
            drop(reader);
            assert!(matches!(
                first.await.unwrap(),
                Err(RpcPeerError::ConnectionClosed(_))
            ));
            assert!(matches!(
                waiting.await,
                Err(RpcPeerError::ConnectionClosed(_))
            ));
            assert!(matches!(
                incoming.recv().await,
                Err(broadcast::error::RecvError::Closed)
            ));
        })
        .await
        .unwrap();
    }

    #[test]
    fn event_overflow_is_explicit_and_early_events_survive_closure() {
        let queue = RpcEventQueue::new(2);
        for value in ["a", "b", "c"] {
            queue.deliver(RpcEvent::Message(value.into()));
        }
        queue.deliver(RpcEvent::Closed("disconnected".into()));
        let mut events = queue.subscribe();
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(1))
        ));
        assert_eq!(events.try_recv().unwrap(), "b");
        assert_eq!(events.try_recv().unwrap(), "c");
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Closed)
        ));
        assert!(matches!(
            queue.subscribe().try_recv(),
            Err(broadcast::error::TryRecvError::Closed)
        ));
    }

    #[tokio::test]
    async fn exhausted_ids_are_rejected_without_reuse() {
        let (peer, _reader, _writer) = pair(1, |_| {});
        peer.next_id.store(u64::MAX, Ordering::Relaxed);
        assert!(matches!(
            peer.request_raw(r#"{"id":1,"method":"never-sent"}"#, Duration::from_secs(1))
                .await,
            Err(RpcPeerError::RequestIdExhausted)
        ));
    }

    #[tokio::test]
    async fn correlates_responses_and_restores_original_raw_ids() {
        timeout(Duration::from_secs(2), async {
            let (peer, mut lines, mut server_writer) = pair(128, |_| {});
            let first = {
                let peer = peer.clone();
                tokio::spawn(async move {
                    peer.request_raw(
                        r#"{"id":"mobile-a","method":"first","params":{"nested":{"id":1}},"future":{"keep":true}}"#, Duration::from_secs(1)
                    )
                    .await
                })
            };
            let second = {
                let peer = peer.clone();
                tokio::spawn(async move {
                    peer.request_raw(r#"{"id":42,"method":"second","params":{"unknown":[1,2,3]}}"#, Duration::from_secs(1))
                        .await
                })
            };

            let request_a: Value = serde_json::from_str(&lines.read_line().await.unwrap().unwrap()).unwrap();
            let request_b: Value = serde_json::from_str(&lines.read_line().await.unwrap().unwrap()).unwrap();
            let id_a = request_a["id"].as_u64().unwrap();
            let id_b = request_b["id"].as_u64().unwrap();
            assert_ne!(id_a, id_b);
            assert_eq!(request_a["params"]["nested"]["id"], 1);
            assert_eq!(request_a["future"]["keep"], true);
            assert_eq!(request_b["params"]["unknown"], json!([1, 2, 3]));

            server_writer
                .write_line(
                    &format!(
                        "{{\"id\":{id_b},\"result\":{{\"method\":\"second\",\"futureResult\":{{\"id\":99}}}},\"unknown\":[true]}}"
                    )
                    ,
                )
                .await
                .unwrap();
            server_writer
                .write_line(
                    &format!(
                        "{{\"id\":{id_a},\"error\":{{\"code\":-1,\"message\":\"nope\",\"futureError\":{{\"id\":7}}}},\"extension\":{{\"keep\":true}}}}"
                    )
                    ,
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
        }).await.unwrap();
    }

    #[tokio::test]
    async fn publishes_raw_notifications_and_server_requests() {
        timeout(Duration::from_secs(2), async {
            let queue = RpcEventQueue::new(128);
            let mut events = queue.subscribe();
            let (_peer, _reader, mut server_writer) = pair(128, move |event| queue.deliver(event));
            let notification = r#" {"method":"item/started","params":{"future":{"id":1}}} "#;
            let request =
                r#"{"id":"approval-1","method":"item/approval","params":{"opaque":[1,2]}}"#;
            server_writer.write_line(notification).await.unwrap();
            server_writer.write_line(request).await.unwrap();

            assert_eq!(events.recv().await.unwrap(), notification);
            assert_eq!(events.recv().await.unwrap(), request);
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn sends_validated_notification_and_response_without_rewriting() {
        timeout(Duration::from_secs(2), async {
            let (peer, mut lines, _server_writer) = pair(128, |_| {});
            let notification = r#" {"method":"event","params":{"future":true}} "#;
            let response = r#"{"id":"approval","result":{"future":[1,2]}}"#;
            peer.send_raw(notification).await.unwrap();
            peer.send_raw(response).await.unwrap();
            assert_eq!(lines.read_line().await.unwrap().unwrap(), notification);
            assert_eq!(lines.read_line().await.unwrap().unwrap(), response);
            assert!(
                peer.send_raw(r#"{"id":1,"method":"not-a-response"}"#)
                    .await
                    .is_err()
            );
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn ignores_unknown_and_late_responses() {
        timeout(Duration::from_secs(2), async {
            let (peer, mut lines, mut server_writer) = pair(128, |_| {});
            let request = {
                let peer = peer.clone();
                tokio::spawn(async move {
                    peer.request_raw(
                        r#"{"id":"caller","method":"wait","params":{}}"#,
                        Duration::from_secs(1),
                    )
                    .await
                })
            };
            let sent: Value =
                serde_json::from_str(&lines.read_line().await.unwrap().unwrap()).unwrap();
            let id = sent["id"].as_u64().unwrap();
            server_writer
                .write_line("{\"id\":999,\"result\":{\"late\":true}}")
                .await
                .unwrap();
            server_writer
                .write_line(&format!("{{\"id\":{id},\"result\":{{\"ok\":true}}}}"))
                .await
                .unwrap();
            let response: Value = serde_json::from_str(&request.await.unwrap().unwrap()).unwrap();
            assert_eq!(response["id"], "caller");
            assert_eq!(response["result"]["ok"], true);
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn timeout_removes_pending_request() {
        timeout(Duration::from_secs(2), async {
            let (peer, mut lines, _server_writer) = pair(128, |_| {});
            let error = peer
                .request_raw(
                    r#"{"id":"timeout","method":"blocked","params":{}}"#,
                    Duration::from_millis(10),
                )
                .await
                .unwrap_err();
            let _ = lines.read_line().await.unwrap().unwrap();
            assert!(
                matches!(error, RpcPeerError::RequestTimeout { method } if method == "blocked")
            );
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn cancellation_removes_pending_request_and_late_response_is_ignored() {
        timeout(Duration::from_secs(2), async {
            let (peer, mut lines, mut server_writer) = pair(128, |_| {});
            let cancelled = {
                let peer = peer.clone();
                tokio::spawn(async move {
                    peer.request_raw(
                        r#"{"id":"cancelled","method":"cancel","params":{}}"#,
                        Duration::from_secs(1),
                    )
                    .await
                })
            };
            let sent: Value =
                serde_json::from_str(&lines.read_line().await.unwrap().unwrap()).unwrap();
            let cancelled_id = sent["id"].as_u64().unwrap();
            cancelled.abort();
            let _ = cancelled.await;

            server_writer
                .write_line(&format!(
                    "{{\"id\":{cancelled_id},\"result\":{{\"late\":true}}}}"
                ))
                .await
                .unwrap();
            let next = {
                let peer = peer.clone();
                tokio::spawn(async move {
                    peer.request_raw(
                        r#"{"id":"next","method":"next","params":{}}"#,
                        Duration::from_secs(1),
                    )
                    .await
                })
            };
            let sent: Value =
                serde_json::from_str(&lines.read_line().await.unwrap().unwrap()).unwrap();
            let next_id = sent["id"].as_u64().unwrap();
            server_writer
                .write_line(&format!("{{\"id\":{next_id},\"result\":{{\"ok\":true}}}}"))
                .await
                .unwrap();
            let response: Value = serde_json::from_str(&next.await.unwrap().unwrap()).unwrap();
            assert_eq!(response["id"], "next");
            assert_eq!(response["result"]["ok"], true);
        })
        .await
        .unwrap();
    }
}
