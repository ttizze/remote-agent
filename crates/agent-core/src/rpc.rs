//! Reconnecting stream adapters. Framing and request correlation live in peer.
use crate::peer::{EventDelivery, PeerError, PeerEvent, RpcPeer, request_line};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, mpsc},
    time::Duration,
};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

type Reply = Result<Value, String>;
struct Connection {
    peer: Mutex<Option<Arc<RpcPeer>>>,
    stop: CancellationToken,
}
#[derive(Clone)]
pub struct Rpc(Arc<Connection>);
pub enum Event {
    Connected(bool, String),
    Message(Value),
}

pub(crate) fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("client runtime")
    })
}
impl Rpc {
    pub fn connect(path: PathBuf, target: Value, events: impl Fn(Event) + Send + 'static) -> Self {
        let target = serde_json::to_string(&target).expect("local target serializes");
        let state = Arc::new(Connection {
            peer: Mutex::new(None),
            stop: CancellationToken::new(),
        });
        let worker = state.clone();
        let events = Arc::new(Mutex::new(events));
        runtime().spawn(async move {
            while !worker.stop.is_cancelled() {
                let connect = async {
                    let stream = tokio::net::UnixStream::connect(&path)
                        .await
                        .map_err(|e| e.to_string())?;
                    let (reader, mut writer) = stream.into_split();
                    writer
                        .write_all(target.as_bytes())
                        .await
                        .map_err(|e| e.to_string())?;
                    writer.write_all(b"\n").await.map_err(|e| e.to_string())?;
                    let mut reader = host_protocol::JsonlReader::new(reader);
                    let ready = reader
                        .read_line()
                        .await
                        .map_err(|e| e.to_string())?
                        .ok_or("Host closed during connection")?;
                    #[derive(serde::Deserialize)]
                    struct Ready {
                        ready: bool,
                    }
                    if !serde_json::from_str::<Ready>(&ready)
                        .map_err(|e| e.to_string())?
                        .ready
                    {
                        return Err("Host is not ready".into());
                    }
                    Ok((reader, writer))
                };
                let result = tokio::select! {
                    _ = worker.stop.cancelled() => break,
                    result = tokio::time::timeout(Duration::from_secs(30), connect) => result.unwrap_or_else(|_| Err("Host connection timed out".into()))
                };
                let reason = match result {
                    Err(error) => error,
                    Ok((reader, writer)) => {
                        let (closed, mut closing) = tokio::sync::mpsc::unbounded_channel();
                        let observer = events.clone();
                        // Prevent events racing ahead of Connected and of the installed request handle.
                        let peer = {
                            let observer_guard = events.lock().unwrap();
                            let peer = RpcPeer::open(reader, writer, Duration::from_secs(30), 8, EventDelivery::Callback(Arc::new(move |event| {
                                match event {
                                    PeerEvent::Message(line) => {
                                        let event = serde_json::from_str(&line).map_err(|e| e.to_string());
                                        match event {
                                            Ok(event) => (observer.lock().unwrap())(Event::Message(event)),
                                            Err(error) => { let _ = closed.send(error); }
                                        }
                                    }
                                    PeerEvent::Closed(reason) => { let _ = closed.send(reason); }
                                }
                            }))).expect("positive framing limit");
                            let peer = Arc::new(peer);
                            *worker.peer.lock().unwrap() = Some(peer.clone());
                            observer_guard(Event::Connected(true, String::new()));
                            peer
                        };
                        let reason = tokio::select! {
                            _ = worker.stop.cancelled() => "接続を閉じました".into(),
                            reason = closing.recv() => reason.unwrap_or_else(|| "Host との接続が切れました".into())
                        };
                        worker.peer.lock().unwrap().take();
                        peer.close();
                        reason
                    }
                };
                (events.lock().unwrap())(Event::Connected(false, reason));
                tokio::select! { _ = worker.stop.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(1)) => {} }
            }
        });
        Self(state)
    }
    pub fn request_async(
        &self,
        method: &str,
        params: Value,
        done: impl FnOnce(Reply) + Send + 'static,
    ) {
        let line = match request_line(method, &params) {
            Ok(line) => line,
            Err(error) => {
                done(Err(error.to_string()));
                return;
            }
        };
        let peer = self.0.peer.lock().unwrap().as_ref().cloned();
        let Some(peer) = peer else {
            done(Err("Host に接続していません".into()));
            return;
        };
        let timeout = Duration::from_secs(if method == "host/transfer" { 150 } else { 30 });
        runtime().spawn(async move {
            peer.request_callback(&line, timeout, move |result| {
                done(
                    result
                        .and_then(|line| crate::peer::response_value(&line))
                        .map_err(error_message),
                );
            })
            .await;
        });
    }
    pub fn request(&self, method: &str, params: Value) -> Reply {
        let (tx, rx) = mpsc::channel();
        self.request_async(method, params, move |result| {
            let _ = tx.send(result);
        });
        rx.recv()
            .unwrap_or_else(|_| Err("Host との接続が切れました".into()))
    }
    pub fn respond(&self, id: Value, result: Value) -> Result<(), String> {
        let peer = self
            .0
            .peer
            .lock()
            .unwrap()
            .clone()
            .ok_or("Host に接続していません")?;
        let line = serde_json::to_string(&serde_json::json!({"id":id,"result":result}))
            .map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::channel();
        runtime().spawn(async move {
            let _ = tx.send(peer.send_raw(line).await.map_err(error_message));
        });
        rx.recv()
            .unwrap_or_else(|_| Err("Host との接続が切れました".into()))
    }
    pub fn close(&self) {
        self.0.stop.cancel();
        if let Some(active) = self.0.peer.lock().unwrap().take() {
            active.close();
        }
    }
}
fn error_message(error: PeerError) -> String {
    match error {
        PeerError::Remote { error } => serde_json::from_str::<Value>(&error)
            .ok()
            .and_then(|v| v["message"].as_str().map(str::to_owned))
            .unwrap_or(error),
        error => error.to_string(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::net::UnixListener;
    use std::{
        io::{BufRead, BufReader, Write},
        net::Shutdown,
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
            matches!(rx.recv_timeout(Duration::from_secs(4)).unwrap(),Event::Message(v) if v["method"] == "turn/started")
        );
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(4)).unwrap(),Event::Message(v) if v["id"] == "approval")
        );
        client
            .respond(json!("approval"), json!({"decision":"decline"}))
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
        client.request_async("thread/resume", json!({"threadId":"t"}), move |value| {
            assert_eq!(value.unwrap()["thread"]["id"], "t");
            tx.send("snapshot").unwrap();
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(4)).unwrap(), "snapshot");
        assert_eq!(rx.recv_timeout(Duration::from_secs(4)).unwrap(), "delta");
        server.join().unwrap();
        client.close();
    }
}
