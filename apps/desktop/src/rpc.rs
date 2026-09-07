use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

type Reply = Result<Value, String>;
type Completion = Box<dyn FnOnce(Reply) + Send>;
struct Connection {
    stream: Option<UnixStream>,
    ready: bool,
    pending: HashMap<u64, Completion>,
}
struct Inner {
    connection: Mutex<Connection>,
    next: AtomicU64,
    stopped: AtomicBool,
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
            connection: Mutex::new(Connection {
                stream: None,
                ready: false,
                pending: HashMap::new(),
            }),
            next: AtomicU64::new(1),
            stopped: AtomicBool::new(false),
        });
        let worker = inner.clone();
        std::thread::spawn(move || {
            while !worker.stopped.load(Ordering::Relaxed) {
                let result = (|| -> Result<(), String> {
                    let mut stream = UnixStream::connect(&path).map_err(|e| e.to_string())?;
                    stream
                        .set_write_timeout(Some(Duration::from_secs(30)))
                        .map_err(|e| e.to_string())?;
                    worker.connection.lock().unwrap().stream =
                        Some(stream.try_clone().map_err(|e| e.to_string())?);
                    stream
                        .set_read_timeout(Some(Duration::from_secs(30)))
                        .map_err(|e| e.to_string())?;
                    writeln!(stream, "{target}").map_err(|e| e.to_string())?;
                    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
                    let mut line = Vec::new();
                    loop {
                        line.clear();
                        // Bound each frame before allocating an untrusted payload in full.
                        let mut limited = (&mut reader).take(256 * 1024 * 1024 + 1);
                        use std::io::Read;
                        let count = limited
                            .read_until(b'\n', &mut line)
                            .map_err(|e| e.to_string())?;
                        if count == 0 {
                            return Err("Host との接続が切れました".into());
                        }
                        if count > 256 * 1024 * 1024 {
                            return Err("受信サイズが上限を超えました".into());
                        }
                        let message: Value =
                            serde_json::from_slice(&line).map_err(|_| "Host の応答が不正です")?;
                        if message["ready"] == true {
                            worker.connection.lock().unwrap().ready = true;
                            stream.set_read_timeout(None).map_err(|e| e.to_string())?;
                            events(Event::Connected(true, String::new()));
                        } else if message.get("method").is_none() && message.get("id").is_some() {
                            if let Some(sender) = message["id"].as_u64().and_then(|id| {
                                worker.connection.lock().unwrap().pending.remove(&id)
                            }) {
                                let result = if message.get("error").is_some() {
                                    Err(message["error"]["message"]
                                        .as_str()
                                        .unwrap_or("Host request failed")
                                        .into())
                                } else {
                                    Ok(message["result"].clone())
                                };
                                sender(result);
                            }
                        } else {
                            events(Event::Message(message));
                        }
                        if worker.stopped.load(Ordering::Relaxed) {
                            return Ok(());
                        }
                    }
                })();
                let reason = result.err().unwrap_or_else(|| "接続を閉じました".into());
                {
                    let mut connection = worker.connection.lock().unwrap();
                    connection.stream = None;
                    connection.ready = false;
                    let pending = std::mem::take(&mut connection.pending);
                    drop(connection);
                    for (_, reply) in pending {
                        reply(Err(reason.clone()));
                    }
                }
                events(Event::Connected(false, reason));
                for _ in 0..10 {
                    if worker.stopped.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
        Self(inner)
    }
    /// Completion executes on the reader before its next notification. The UI queue therefore
    /// preserves wire order when a snapshot reply is immediately followed by streaming deltas.
    pub(crate) fn request_async(
        &self,
        method: &str,
        params: Value,
        done: impl FnOnce(Reply) + Send + 'static,
    ) {
        let client = self.clone();
        let method = method.to_owned();
        std::thread::spawn(move || {
            let id = client.0.next.fetch_add(1, Ordering::Relaxed);
            let (cancel_tx, cancel_rx) = mpsc::channel();
            let completion: Completion = Box::new(move |reply| {
                done(reply);
                let _ = cancel_tx.send(());
            });
            {
                let mut connection = client.0.connection.lock().unwrap();
                let result = if !connection.ready {
                    Err("Host に接続していません".into())
                } else if let Some(stream) = connection.stream.as_mut() {
                    writeln!(
                        stream,
                        "{}",
                        json!({"id":id,"method":method,"params":params})
                    )
                    .map_err(|e| e.to_string())
                } else {
                    Err("Host に接続していません".into())
                };
                if let Err(error) = result {
                    drop(connection);
                    completion(Err(error));
                    return;
                }
                connection.pending.insert(id, completion);
            }
            let timeout = if method == "host/transfer" { 150 } else { 30 };
            if cancel_rx
                .recv_timeout(Duration::from_secs(timeout))
                .is_err()
            {
                let completion = client.0.connection.lock().unwrap().pending.remove(&id);
                if let Some(done) = completion {
                    done(Err(format!(
                        "{method}: 応答を確認できません。状態を更新してください。"
                    )));
                }
            }
        });
    }
    pub(crate) fn request(&self, method: &str, params: Value) -> Reply {
        let (tx, rx) = mpsc::channel();
        self.request_async(method, params, move |result| {
            let _ = tx.send(result);
        });
        rx.recv()
            .unwrap_or_else(|_| Err("Host との接続が切れました".into()))
    }
    pub(crate) fn respond(&self, id: Value, result: Value) -> Result<(), String> {
        let mut connection = self.0.connection.lock().unwrap();
        if !connection.ready {
            return Err("Host に接続していません".into());
        }
        let stream = connection
            .stream
            .as_mut()
            .ok_or("Host に接続していません")?;
        writeln!(stream, "{}", json!({"id":id,"result":result})).map_err(|e| e.to_string())
    }
    pub(crate) fn close(&self) {
        self.0.stopped.store(true, Ordering::Relaxed);
        if let Some(stream) = self.0.connection.lock().unwrap().stream.as_ref() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
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
