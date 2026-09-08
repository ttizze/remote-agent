use crate::Error;
use host_protocol::{DEFAULT_MAX_MESSAGE_BYTES, RpcEventQueue, RpcPeer as JsonlPeer, RpcPeerError};
use std::time::Duration;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::broadcast,
};

/// Codex process adapter. Correlation, cancellation, and framing live in host-protocol.
pub(crate) struct RpcPeer {
    peer: JsonlPeer,
    events: RpcEventQueue,
    request_timeout: Duration,
}
impl RpcPeer {
    pub(crate) fn open<R, W>(reader: R, writer: W, request_timeout: Duration) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let events = RpcEventQueue::new(128);
        let sink = events.clone();
        // This connection serves all authorized device sessions concurrently.
        let peer = JsonlPeer::open(
            reader,
            writer,
            DEFAULT_MAX_MESSAGE_BYTES,
            128,
            move |event| sink.deliver(event),
        );
        Self {
            peer,
            events,
            request_timeout,
        }
    }
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<String> {
        self.events.subscribe()
    }
    pub(crate) async fn request_raw(&self, line: &str) -> Result<String, Error> {
        self.peer
            .request_raw(line, self.request_timeout)
            .await
            .map_err(codex_error)
    }
    pub(crate) async fn send_raw(&self, line: &str) -> Result<(), Error> {
        self.peer.send_raw(line).await.map_err(codex_error)
    }
}
fn codex_error(error: RpcPeerError) -> Error {
    match error {
        RpcPeerError::InvalidMessage(reason) => Error::InvalidMessage(reason),
        RpcPeerError::ConnectionClosed(reason) => Error::ConnectionClosed(reason),
        RpcPeerError::RequestTimeout { method } => Error::RequestTimeout { method },
        RpcPeerError::RequestIdExhausted => Error::InvalidMessage(error.to_string()),
    }
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
