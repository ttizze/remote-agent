use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex as StdMutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use host_protocol::{
    JsonlReader, JsonlWriter, RpcMessage, RpcMessageKind, classify_message, raw_object,
};
use serde_json::{Value, value::RawValue};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{Semaphore, broadcast, mpsc, oneshot},
    time::timeout,
};

use crate::{MobileClientError, Notification, ServerRequest};

const MAX_IN_FLIGHT_REQUESTS: usize = 1_024;
const MAX_OUTBOUND_QUEUE_MESSAGES: usize = 4_096;

type PendingResponse = oneshot::Sender<Result<Value, MobileClientError>>;
type SharedState = Arc<StdMutex<PeerState>>;

struct PeerState {
    pending: HashMap<String, PendingResponse>,
    terminal: Option<String>,
    notifications: Option<broadcast::Sender<Notification>>,
    server_requests: Option<broadcast::Sender<ServerRequest>>,
    initial_notifications: Option<broadcast::Receiver<Notification>>,
    initial_requests: Option<broadcast::Receiver<ServerRequest>>,
}

/// Correlates bidirectional Codex JSONL traffic on one authenticated relay
/// stream.
///
/// The transport never deserializes params, result, error, or extension data.
/// It only classifies top-level routing keys. Requests made through the Rust
/// API construct a JSON object from the caller's `Value`; inbound events and
/// Host requests remain their original source lines.
pub(crate) struct RpcPeer {
    next_id: AtomicU64,
    outbound: mpsc::Sender<String>,
    state: SharedState,
    permits: Arc<Semaphore>,
    request_timeout: Duration,
}

impl RpcPeer {
    pub(crate) fn open<S>(
        stream: S,
        max_message_bytes: usize,
        request_timeout: Duration,
    ) -> Result<Self, MobileClientError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        if max_message_bytes == 0 {
            return Err(MobileClientError::InvalidConfig(
                "max_message_bytes must be positive",
            ));
        }

        let (outbound, outbound_rx) = mpsc::channel(MAX_OUTBOUND_QUEUE_MESSAGES);
        let (notifications, initial_notifications) =
            broadcast::channel(MAX_OUTBOUND_QUEUE_MESSAGES);
        let (server_requests, initial_requests) = broadcast::channel(MAX_OUTBOUND_QUEUE_MESSAGES);
        let state = Arc::new(StdMutex::new(PeerState {
            pending: HashMap::new(),
            terminal: None,
            notifications: Some(notifications),
            server_requests: Some(server_requests),
            initial_notifications: Some(initial_notifications),
            initial_requests: Some(initial_requests),
        }));
        let permits = Arc::new(Semaphore::new(MAX_IN_FLIGHT_REQUESTS));
        let (reader, writer) = tokio::io::split(stream);
        tokio::spawn(write_loop(
            writer,
            outbound_rx,
            state.clone(),
            permits.clone(),
            max_message_bytes,
        ));
        tokio::spawn(read_loop(
            reader,
            state.clone(),
            permits.clone(),
            max_message_bytes,
        ));

        Ok(Self {
            next_id: AtomicU64::new(1),
            outbound,
            state,
            permits,
            request_timeout,
        })
    }

    pub(crate) fn subscribe_notifications(&self) -> broadcast::Receiver<Notification> {
        notification_receiver(&self.state)
    }

    pub(crate) fn subscribe_server_requests(&self) -> broadcast::Receiver<ServerRequest> {
        server_request_receiver(&self.state)
    }

    pub(crate) async fn request(
        &self,
        method: String,
        params: Value,
    ) -> Result<Value, MobileClientError> {
        self.request_raw(method, serde_json::to_string(&params)?)
            .await
    }

    pub(crate) async fn request_raw(
        &self,
        method: String,
        params: String,
    ) -> Result<Value, MobileClientError> {
        ensure_active(&self.state)?;
        let numeric_id = allocate_request_id(&self.next_id)?;
        let id = numeric_id.to_string();
        let state = &self.state;
        let operation = async {
            let _permit = self
                .permits
                .acquire()
                .await
                .map_err(|_| disconnected_or(state, "request limiter closed"))?;

            let (tx, rx) = oneshot::channel();
            register_pending(state, id.clone(), tx)?;
            let _registration = PendingRegistration {
                state: state.clone(),
                id,
            };

            ensure_active(state)?;
            let line = request_line(numeric_id, &method, &params)?;
            self.outbound
                .send(line)
                .await
                .map_err(|_| disconnected_or(state, "JSONL writer stopped"))?;
            rx.await
                .map_err(|_| disconnected_or(state, "JSONL reader stopped"))?
        };

        timeout(self.request_timeout, operation)
            .await
            .map_err(|_| MobileClientError::RequestTimeout { id: numeric_id })?
    }

    pub(crate) async fn respond_result(
        &self,
        id: String,
        result: Value,
    ) -> Result<(), MobileClientError> {
        self.respond_raw(id, "result", serde_json::to_string(&result)?)
            .await
    }

    pub(crate) async fn respond_error(
        &self,
        id: String,
        error: Value,
    ) -> Result<(), MobileClientError> {
        self.respond_raw(id, "error", serde_json::to_string(&error)?)
            .await
    }

    pub(crate) async fn respond_raw(
        &self,
        id: String,
        field: &'static str,
        payload: String,
    ) -> Result<(), MobileClientError> {
        ensure_active(&self.state)?;
        if field != "result" && field != "error" {
            return Err(MobileClientError::Protocol(
                "response field must be result or error".to_owned(),
            ));
        }
        let line = response_line(&id, field, &payload)?;
        self.outbound
            .send(line)
            .await
            .map_err(|_| disconnected_or(&self.state, "JSONL writer stopped"))
    }
}

/// Removes a pending request when the request future is cancelled or times
/// out. The reader may already have removed the same ID; removal is a no-op.
struct PendingRegistration {
    state: SharedState,
    id: String,
}

impl Drop for PendingRegistration {
    fn drop(&mut self) {
        lock_state(&self.state).pending.remove(&self.id);
    }
}

fn lock_state(state: &SharedState) -> MutexGuard<'_, PeerState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn notification_receiver(state: &SharedState) -> broadcast::Receiver<Notification> {
    let mut state = lock_state(state);
    if let Some(receiver) = state.initial_notifications.take() {
        return receiver;
    }
    state
        .notifications
        .as_ref()
        .map(broadcast::Sender::subscribe)
        .unwrap_or_else(closed_receiver)
}

fn server_request_receiver(state: &SharedState) -> broadcast::Receiver<ServerRequest> {
    let mut state = lock_state(state);
    if let Some(receiver) = state.initial_requests.take() {
        return receiver;
    }
    state
        .server_requests
        .as_ref()
        .map(broadcast::Sender::subscribe)
        .unwrap_or_else(closed_receiver)
}

fn closed_receiver<T: Clone>() -> broadcast::Receiver<T> {
    let (sender, receiver) = broadcast::channel(1);
    drop(sender);
    receiver
}

fn terminal_reason(state: &SharedState) -> Option<String> {
    lock_state(state).terminal.clone()
}

fn ensure_active(state: &SharedState) -> Result<(), MobileClientError> {
    terminal_reason(state).map_or(Ok(()), |reason| {
        Err(MobileClientError::Disconnected(reason))
    })
}

fn disconnected_or(state: &SharedState, fallback: &str) -> MobileClientError {
    terminal_reason(state).map_or_else(
        || MobileClientError::Disconnected(fallback.to_owned()),
        MobileClientError::Disconnected,
    )
}

fn register_pending(
    state: &SharedState,
    id: String,
    response: PendingResponse,
) -> Result<(), MobileClientError> {
    let mut state = lock_state(state);
    if let Some(reason) = state.terminal.as_ref() {
        return Err(MobileClientError::Disconnected(reason.clone()));
    }
    state.pending.insert(id, response);
    Ok(())
}

fn allocate_request_id(next_id: &AtomicU64) -> Result<u64, MobileClientError> {
    let mut current = next_id.load(Ordering::Relaxed);
    loop {
        if current == u64::MAX {
            return Err(MobileClientError::RequestIdExhausted);
        }
        match next_id.compare_exchange_weak(
            current,
            current + 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return Ok(current),
            Err(observed) => current = observed,
        }
    }
}

fn request_line(id: u64, method: &str, params: &str) -> Result<String, MobileClientError> {
    #[derive(serde::Serialize)]
    struct Request<'a> {
        id: u64,
        method: &'a str,
        params: &'a RawValue,
    }
    Ok(serde_json::to_string(&Request {
        id,
        method,
        params: serde_json::from_str(params)?,
    })?)
}

fn response_line(
    id: &str,
    field: &'static str,
    payload: &str,
) -> Result<String, MobileClientError> {
    let id: &RawValue = serde_json::from_str(id)?;
    let payload: &RawValue = serde_json::from_str(payload)?;
    let mut object = BTreeMap::new();
    object.insert("id", id);
    object.insert(field, payload);
    Ok(serde_json::to_string(&object)?)
}

async fn write_loop<W>(
    writer: W,
    mut outbound: mpsc::Receiver<String>,
    state: SharedState,
    permits: Arc<Semaphore>,
    max_message_bytes: usize,
) where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut writer = JsonlWriter::with_max_message_bytes(writer, max_message_bytes);
    while let Some(line) = outbound.recv().await {
        if let Err(error) = writer.write_line(&line).await {
            terminate(&state, &permits, error.to_string());
            return;
        }
    }
    terminate(&state, &permits, "JSONL writer stopped".to_owned());
}

async fn read_loop<R>(
    reader: R,
    state: SharedState,
    permits: Arc<Semaphore>,
    max_message_bytes: usize,
) where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut reader = JsonlReader::with_max_message_bytes(reader, max_message_bytes);
    loop {
        let Some(line) = (match reader.read_line().await {
            Ok(line) => line,
            Err(error) => {
                terminate(&state, &permits, error.to_string());
                return;
            }
        }) else {
            terminate(&state, &permits, "relay channel closed".to_owned());
            return;
        };

        let message = match classify_message(&line) {
            Ok(message) => message,
            Err(error) => {
                terminate(&state, &permits, error.to_string());
                return;
            }
        };
        match message.kind() {
            RpcMessageKind::Response => handle_response(&state, message),
            RpcMessageKind::Notification => {
                let state = lock_state(&state);
                if let Some(sender) = state.notifications.as_ref() {
                    let _ = sender.send(line);
                }
            }
            RpcMessageKind::Request => {
                let state = lock_state(&state);
                if let Some(sender) = state.server_requests.as_ref() {
                    let _ = sender.send(line);
                }
            }
        }
    }
}

fn handle_response(state: &SharedState, message: RpcMessage<'_>) {
    let Some(id) = message.raw_id() else {
        return;
    };
    let line = message.raw_line();
    let Some(tx) = lock_state(state).pending.remove(id) else {
        return; // Late or duplicate response after timeout.
    };

    let result = match raw_object(line) {
        Ok(object) if object.contains_key("result") => object
            .get("result")
            .and_then(|raw| serde_json::from_str::<Value>(raw.get()).ok())
            .ok_or_else(|| MobileClientError::Protocol("invalid result JSON".to_owned())),
        Ok(object) if object.contains_key("error") => Err(MobileClientError::Remote {
            error: object["error"].get().to_owned(),
        }),
        Ok(_) => Err(MobileClientError::Protocol(
            "response has neither result nor error".to_owned(),
        )),
        Err(error) => Err(error.into()),
    };
    let _ = tx.send(result);
}

fn terminate(state: &SharedState, permits: &Arc<Semaphore>, reason: String) {
    let (pending, notifications, server_requests) = {
        let mut state = lock_state(state);
        if state.terminal.is_some() {
            return;
        }
        state.terminal = Some(reason.clone());
        (
            std::mem::take(&mut state.pending),
            state.notifications.take(),
            state.server_requests.take(),
        )
    };

    permits.close();
    drop(notifications);
    drop(server_requests);
    for tx in pending.into_values() {
        let _ = tx.send(Err(MobileClientError::Disconnected(reason.clone())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructed_request_does_not_change_nested_values() {
        let line = request_line(
            7,
            "codex/custom",
            r#"{"future": {"id": "nested"}, "text": "hello"}"#,
        )
        .unwrap();
        let object = raw_object(&line).unwrap();
        assert_eq!(
            object["params"].get(),
            r#"{"future": {"id": "nested"}, "text": "hello"}"#
        );
        let method = "custom/\"日本語\\method";
        let line = request_line(u64::MAX, method, "null").unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["id"], u64::MAX);
        assert_eq!(value["method"], method);
        assert!(value["params"].is_null());
        assert!(request_line(1, method, "{} []").is_err());
    }

    #[test]
    fn response_builder_keeps_raw_payload_and_id_values() {
        let line = response_line(
            r#""request-7""#,
            "result",
            r#"{"unknown":[1,{"future":true}]}"#,
        )
        .unwrap();
        let object = raw_object(&line).unwrap();
        assert_eq!(object["id"].get(), r#""request-7""#);
        assert_eq!(object["result"].get(), r#"{"unknown":[1,{"future":true}]}"#);
        let error = r#"{ "code": -1, "message": "失敗", "data": [null,{"id":7}] }"#;
        let line = response_line("7", "error", error).unwrap();
        let object = raw_object(&line).unwrap();
        assert_eq!(object["id"].get(), "7");
        assert_eq!(object["error"].get(), error);
        assert!(response_line("7 8", "result", "null").is_err());
        assert!(response_line("7", "error", "{").is_err());
    }

    #[test]
    fn request_ids_stop_at_exhaustion_without_wrapping() {
        let next_id = AtomicU64::new(u64::MAX - 1);
        assert_eq!(allocate_request_id(&next_id).unwrap(), u64::MAX - 1);
        assert!(matches!(
            allocate_request_id(&next_id),
            Err(MobileClientError::RequestIdExhausted)
        ));
        assert_eq!(next_id.load(Ordering::Relaxed), u64::MAX);
    }
}
