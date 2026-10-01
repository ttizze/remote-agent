//! Ordered inbound delivery and acknowledged outbound writes share one shutdown path.
use super::{
    JsonlReader, JsonlWriter, Outbound, PeerError, PeerEvent, Reply, RpcMessage, RpcMessageKind,
    State, WriterStatus, invalid,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{Semaphore, mpsc, watch},
};
use tokio_util::sync::CancellationToken;

pub(super) enum Termination {
    Requested,
    Failed(String),
}

pub(super) async fn read_loop<R: AsyncRead + Unpin>(
    mut reader: JsonlReader<R>,
    state: Arc<Mutex<State>>,
    permits: Arc<Semaphore>,
    stop: CancellationToken,
) {
    let reason = loop {
        let line = tokio::select! {
            _ = stop.cancelled() => break Termination::Requested,
            result = reader.read_line() => match result { Ok(Some(line)) => line, Ok(None) => break Termination::Failed("JSONL stream reached EOF".into()), Err(error) => break Termination::Failed(error.to_string()) }
        };
        if line.trim().is_empty() {
            continue;
        }
        let message = match RpcMessage::parse(&line) {
            Ok(message) => message,
            Err(error) => break Termination::Failed(error.to_string()),
        };
        let mut received = state.lock().unwrap();
        received.sequence += 1;
        let sequence = received.sequence;
        if message.kind() == RpcMessageKind::Response {
            let id = message
                .raw_id()
                .and_then(|id| serde_json::from_str::<u64>(id).ok());
            let pending = {
                let pending = id.and_then(|id| received.pending.remove(&id));
                if let Some(events) = &received.events {
                    let _ = events.send(PeerEvent::Response {
                        sequence,
                        request_id: id,
                        method: pending.as_ref().map(|pending| pending.method.clone()),
                    });
                }
                pending
            };
            drop(received);
            if let Some(pending) = pending {
                if let Some(error) = message.raw_error() {
                    crate::diagnostics::rpc_error(&pending.method, id, error);
                }
                let response = match pending.original_id {
                    Some(original_id) => message.rewrite_id(&original_id).map_err(invalid),
                    _ => Ok(line),
                };
                let _ = pending
                    .complete
                    .send(response.map(|value| Reply { sequence, value }));
            }
        } else {
            crate::diagnostics::notification(&message);
            if let Some(events) = &received.events {
                let _ = events.send(PeerEvent::Message(Reply {
                    sequence,
                    value: Arc::from(line.as_str()),
                }));
            }
        }
    };
    terminate(&state, &permits, &stop, reason);
}
pub(super) async fn write_loop<W: AsyncWrite + Unpin>(
    writer: W,
    mut outgoing: mpsc::Receiver<Outbound>,
    state: Arc<Mutex<State>>,
    permits: Arc<Semaphore>,
    stop: CancellationToken,
    maximum: usize,
    finished: watch::Sender<WriterStatus>,
) {
    let mut writer = JsonlWriter::with_max_message_bytes(writer, maximum);
    let reason = loop {
        let line = tokio::select! { _ = stop.cancelled() => break Termination::Requested, line = outgoing.recv() => match line { Some(line) => line, None => break Termination::Requested } };
        let result = tokio::select! { _ = stop.cancelled() => break Termination::Requested, result = writer.write_line(&line.line) => result };
        if let Err(error) = result {
            break Termination::Failed(error.to_string());
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
pub(super) fn terminate(
    state: &Mutex<State>,
    permits: &Semaphore,
    stop: &CancellationToken,
    termination: Termination,
) {
    let (reason, failed) = match termination {
        Termination::Requested => ("peer closed".to_owned(), false),
        Termination::Failed(reason) => (reason, true),
    };
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
        if failed {
            tracing::error!(target: "bex", operation = %(&pending.method), message = %reason);
        }
        let _ = pending
            .complete
            .send(Err(PeerError::ConnectionClosed(reason.clone())));
    }
}
