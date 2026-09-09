use agent_client::operations::AgentClient;
use host_protocol::{
    DEFAULT_MAX_MESSAGE_BYTES, HOST_REQUEST_LIMIT, RpcEvent, RpcPeer, rpc_runtime,
};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::watch,
};

type Reply = Result<Value, String>;
struct Inner {
    connection: Mutex<Option<Arc<RpcPeer>>>,
    stopped: watch::Sender<bool>,
}
#[derive(Clone)]
pub(crate) struct Rpc(Arc<Inner>);
pub(crate) enum Event {
    Connected(bool, String),
    Message(Value),
}
impl Rpc {
    pub(crate) fn connect(
        path: PathBuf,
        target: Value,
        events: impl Fn(Event) + Send + 'static,
    ) -> Self {
        let inner = Arc::new(Inner {
            connection: Mutex::new(None),
            stopped: watch::channel(false).0,
        });
        let runtime = match rpc_runtime() {
            Ok(runtime) => runtime,
            Err(reason) => {
                events(Event::Connected(false, reason.into()));
                return Self(inner);
            }
        };
        let worker = inner.clone();
        let events = Arc::new(Mutex::new(events));
        let mut stopped = inner.stopped.subscribe();
        runtime.spawn(async move {
            loop {
                if *stopped.borrow() {
                    return;
                }
                let result = tokio::select! {
                    biased;
                    _ = stopped.changed() => return,
                    result = connect_stream(&path, &target) => result,
                };
                let reason = match result {
                    Ok((reader, writer)) => {
                        let (closed, mut closure) = watch::channel(None);
                        let received = events.clone();
                        // Publish readiness before any event from the new reader reaches the UI.
                        {
                            let emit = events.lock().unwrap();
                            let peer = RpcPeer::open(
                                reader,
                                writer,
                                DEFAULT_MAX_MESSAGE_BYTES,
                                HOST_REQUEST_LIMIT,
                                move |event| match event {
                                    RpcEvent::Message(line) => {
                                        if let Ok(value) = serde_json::from_str(&line) {
                                            received.lock().unwrap()(Event::Message(value));
                                        }
                                    }
                                    RpcEvent::Closed(reason) => {
                                        closed.send_replace(Some(reason));
                                    }
                                },
                            );
                            *worker.connection.lock().unwrap() = Some(Arc::new(peer));
                            emit(Event::Connected(true, String::new()));
                        }
                        tokio::select! {
                            biased;
                            _ = stopped.changed() => {
                                let peer = worker.connection.lock().unwrap().take();
                                if let Some(peer) = peer { peer.close(); }
                                return;
                            }
                            _ = closure.changed() => {}
                        }
                        closure
                            .borrow()
                            .clone()
                            .unwrap_or_else(|| "Host との接続が切れました".into())
                    }
                    Err(reason) => reason,
                };
                let peer = worker.connection.lock().unwrap().take();
                if let Some(peer) = peer {
                    peer.close();
                }
                events.lock().unwrap()(Event::Connected(false, reason));
                tokio::select! {
                    biased;
                    _ = stopped.changed() => return,
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }
        });
        Self(inner)
    }

    fn connection(
        &self,
        deadline: Duration,
    ) -> Result<(&'static tokio::runtime::Runtime, AgentClient), String> {
        let peer = self
            .0
            .connection
            .lock()
            .unwrap()
            .clone()
            .ok_or("Host に接続していません")?;
        Ok((rpc_runtime()?, AgentClient::new(peer, deadline)))
    }

    /// The shared reader completes a snapshot before its following notification.
    pub(crate) fn request_async(
        &self,
        method: &'static str,
        params: Value,
        done: impl FnOnce(Reply) + Send + 'static,
    ) {
        let deadline = Duration::from_secs(if method == "host/transfer" { 150 } else { 30 });
        let (runtime, client) = match self.connection(deadline) {
            Ok(connection) => connection,
            Err(error) => {
                done(Err(error));
                return;
            }
        };
        runtime.spawn(async move {
            client
                .request_with(method, &params, move |result| {
                    done(result.map_err(|error| error.to_string()))
                })
                .await;
        });
    }
    pub(crate) fn request(&self, method: &'static str, params: Value) -> Reply {
        let (tx, rx) = mpsc::channel();
        self.request_async(method, params, move |result| {
            let _ = tx.send(result);
        });
        rx.recv()
            .unwrap_or_else(|_| Err("Host との接続が切れました".into()))
    }
    pub(crate) fn agent_async<T, F, Fut>(
        &self,
        operation: F,
        done: impl FnOnce(Result<T, String>) + Send + 'static,
    ) where
        T: Send + 'static,
        F: FnOnce(agent_client::operations::AgentClient) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, agent_client::operations::AgentError>>
            + Send
            + 'static,
    {
        let (runtime, client) = match self.connection(Duration::from_secs(30)) {
            Ok(connection) => connection,
            Err(error) => {
                done(Err(error));
                return;
            }
        };
        runtime.spawn(async move {
            done(operation(client).await.map_err(|error| error.to_string()));
        });
    }

    pub(crate) fn read_thread(&self, thread_id: String, done: impl FnOnce(Reply) + Send + 'static) {
        let (runtime, client) = match self.connection(Duration::from_secs(30)) {
            Ok(connection) => connection,
            Err(error) => {
                done(Err(error));
                return;
            }
        };
        runtime.spawn(async move {
            client
                .read_thread_with(thread_id, true, move |result| {
                    done(result.map_err(|error| error.to_string()))
                })
                .await;
        });
    }

    pub(crate) fn close(&self) {
        self.0.stopped.send_replace(true);
        let peer = self.0.connection.lock().unwrap().take();
        if let Some(peer) = peer {
            peer.close();
        }
    }
}

async fn connect_stream(
    path: &std::path::Path,
    target: &Value,
) -> Result<
    (
        BufReader<tokio::net::unix::OwnedReadHalf>,
        tokio::net::unix::OwnedWriteHalf,
    ),
    String,
> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let stream = UnixStream::connect(path).await.map_err(|e| e.to_string())?;
        let (reader, mut writer) = stream.into_split();
        writer
            .write_all(format!("{target}\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(reader);
        let mut line = Vec::new();
        let count = (&mut reader)
            .take(DEFAULT_MAX_MESSAGE_BYTES as u64 + 1)
            .read_until(b'\n', &mut line)
            .await
            .map_err(|e| e.to_string())?;
        if count > DEFAULT_MAX_MESSAGE_BYTES {
            return Err("受信サイズが上限を超えました".into());
        }
        let ready: Value = serde_json::from_slice(&line).map_err(|_| "Host の応答が不正です")?;
        if ready["ready"] != true {
            return Err(ready["error"]
                .as_str()
                .unwrap_or("Host に接続できません")
                .into());
        }
        // Keep BufReader's read-ahead bytes: events may arrive with the handshake.
        Ok((reader, writer))
    })
    .await
    .map_err(|_| "Host への接続がタイムアウトしました".to_owned())?
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        io::{BufRead, BufReader, Write},
        net::Shutdown,
        os::unix::net::UnixListener,
        sync::mpsc,
    };
    #[test]
    fn reconnect_fails_pending_work_without_replaying_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            first
                .set_read_timeout(Some(Duration::from_secs(4)))
                .unwrap();
            let mut reader = BufReader::new(first.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&line).unwrap(),
                json!({"target":"local"})
            );
            writeln!(first, "{}", json!({"ready":true})).unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&line).unwrap()["method"],
                "turn/start"
            );
            first.shutdown(Shutdown::Both).unwrap();
            let (mut second, _) = listener.accept().unwrap();
            second
                .set_read_timeout(Some(Duration::from_millis(300)))
                .unwrap();
            let mut reader = BufReader::new(second.try_clone().unwrap());
            line.clear();
            reader.read_line(&mut line).unwrap();
            writeln!(second, "{}", json!({"ready":true})).unwrap();
            line.clear();
            assert!(
                reader.read_line(&mut line).is_err(),
                "turn/start must not replay after reconnect"
            );
        });
        let client = Rpc::connect(path, json!({"target":"local"}), move |event| {
            if let Event::Connected(true, _) = event {
                let _ = ready_tx.send(());
            }
        });
        ready_rx.recv_timeout(Duration::from_secs(4)).unwrap();
        assert!(
            client
                .request("turn/start", json!({"threadId":"t","input":[]}))
                .is_err()
        );
        ready_rx.recv_timeout(Duration::from_secs(4)).unwrap();
        server.join().unwrap();
        client.close();
    }
    #[test]
    fn notifications_and_server_requests_survive_before_rpc_reply() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (tx, rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(4)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            writeln!(stream, "{}", json!({"ready":true})).unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            for message in [
                json!({"method":"turn/started","params":{"threadId":"t","turn":{"id":"turn","items":[]}}}),
                json!({"id":"approval","method":"item/commandExecution/requestApproval","params":{"threadId":"t","turnId":"turn"}}),
                json!({"id":request["id"],"result":{"turn":{"id":"turn"}}}),
            ] {
                writeln!(stream, "{message}").unwrap();
            }
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&line).unwrap(),
                json!({"id":"approval","result":{"decision":"decline"}})
            );
        });
        let client = Rpc::connect(path, json!({"target":"local"}), move |event| {
            let _ = tx.send(event);
        });
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(4)).unwrap(),
            Event::Connected(true, _)
        ));
        assert_eq!(
            client
                .request("turn/start", json!({"threadId":"t"}))
                .unwrap()["turn"]["id"],
            "turn"
        );
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(4)).unwrap(),Event::Message(v) if v["method"]=="turn/started")
        );
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(4)).unwrap(),Event::Message(v) if v["id"]=="approval")
        );
        let (responded, response) = mpsc::channel();
        client.agent_async(
            |agent| async move {
                agent.respond(
                    &json!({"id":"approval","method":"item/commandExecution/requestApproval","params":{}}),
                    agent_client::operations::RequestAnswer::Decision { index: 2 },
                ).await
            },
            move |result| { responded.send(result).unwrap(); },
        );
        response
            .recv_timeout(Duration::from_secs(4))
            .unwrap()
            .unwrap();
        server.join().unwrap();
        client.close();
    }
    #[test]
    fn snapshot_completion_enters_ui_queue_before_following_delta() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (tx, rx) = mpsc::channel();
        let events = tx.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(4)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            writeln!(stream, "{}", json!({"ready":true})).unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            writeln!(
                stream,
                "{}",
                json!({"id":request["id"],"result":{"thread":{"id":"t","turns":[]}}})
            )
            .unwrap();
            writeln!(stream,"{}",json!({"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"turn","itemId":"a","delta":"最新"}})).unwrap();
        });
        let client = Rpc::connect(path, json!({"target":"local"}), move |event| {
            let label = match event {
                Event::Connected(true, _) => "ready",
                Event::Message(_) => "delta",
                _ => "closed",
            };
            let _ = events.send(label);
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(4)).unwrap(), "ready");
        client.read_thread("t".into(), move |value| {
            assert_eq!(value.unwrap()["thread"]["id"], "t");
            tx.send("snapshot").unwrap();
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(4)).unwrap(), "snapshot");
        assert_eq!(rx.recv_timeout(Duration::from_secs(4)).unwrap(), "delta");
        server.join().unwrap();
        client.close();
    }
}
