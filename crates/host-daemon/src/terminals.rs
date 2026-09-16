//! Connection-owned PTYs. The private supervisor pipe carries terminal I/O;
//! only this owner publishes events and grants access to a process handle.
use crate::host_rpc::routing::{SessionId, SessionRouter};
use agent_core::{client::TerminalSize, peer::JsonlReader};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bex_process::{PtyCommand, PtyEvent};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::{
    io::AsyncWriteExt,
    sync::{mpsc, oneshot, watch},
};
use tokio_util::sync::CancellationToken;

type Receipt = oneshot::Sender<Result<(), String>>;
struct Command {
    action: Action,
    complete: Receipt,
}
enum Action {
    Write(Vec<u8>),
    Resize(TerminalSize),
}
struct Record {
    owner: SessionId,
    cwd: PathBuf,
    input: mpsc::Sender<Command>,
    stop: CancellationToken,
    finished: watch::Receiver<bool>,
}
#[derive(Default)]
pub(crate) struct Terminals {
    records: Arc<Mutex<HashMap<String, Record>>>,
}
impl Drop for Terminals {
    fn drop(&mut self) {
        for record in self.records.lock().unwrap().values() {
            record.stop.cancel();
        }
    }
}
impl Terminals {
    pub(crate) async fn start(
        &self,
        router: SessionRouter,
        owner: SessionId,
        handle: String,
        cwd: String,
        size: TerminalSize,
    ) -> Result<Value, String> {
        if handle.is_empty() || handle.len() > 256 || size.rows == 0 || size.cols == 0 {
            return Err("invalid terminal handle or size".into());
        }
        let cwd = tokio::fs::canonicalize(cwd)
            .await
            .map_err(|error| error.to_string())?;
        if !cwd.is_dir() {
            return Err("terminal directory is unavailable".into());
        }
        let (input, receiver) = mpsc::channel(32);
        let (complete, finished) = watch::channel(false);
        let stop = CancellationToken::new();
        {
            let mut records = self.records.lock().unwrap();
            router.ensure_session(owner)?;
            if records.len() >= 32 {
                return Err("terminal capacity reached".into());
            }
            if records.contains_key(&handle) {
                return Err("terminal handle is already in use".into());
            }
            records.insert(
                handle.clone(),
                Record {
                    owner,
                    cwd: cwd.clone(),
                    input,
                    stop: stop.clone(),
                    finished,
                },
            );
        }
        let cancel_start = stop.clone().drop_guard();
        let (ready, started) = oneshot::channel();
        let records = Arc::downgrade(&self.records);
        tokio::spawn(async move {
            let _finished = scopeguard::guard((), |_| {
                if let Some(records) = records.upgrade() {
                    records.lock().unwrap().remove(&handle);
                }
                complete.send_replace(true);
            });
            let worker = Worker {
                router,
                owner,
                handle: handle.clone(),
                stop,
                input: receiver,
                ready: Some(ready),
            };
            worker.run(cwd, size).await;
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), started)
            .await
            .map_err(|_| "terminal startup timed out")?
            .map_err(|_| "terminal startup stopped")??;
        cancel_start.disarm();
        Ok(json!({}))
    }
    pub(crate) async fn request(
        &self,
        owner: SessionId,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Params {
            process_handle: String,
            size: Option<TerminalSize>,
            delta_base64: Option<String>,
        }
        let params: Params = serde_json::from_value(params).map_err(|error| error.to_string())?;
        let (input, stop, mut finished) = {
            let records = self.records.lock().unwrap();
            let record = records
                .get(&params.process_handle)
                .ok_or("terminal handle is unavailable")?;
            if record.owner != owner {
                return Err("terminal belongs to another connection".into());
            }
            (
                record.input.clone(),
                record.stop.clone(),
                record.finished.clone(),
            )
        };
        if method == "process/kill" {
            stop.cancel();
            while !*finished.borrow_and_update() {
                finished
                    .changed()
                    .await
                    .map_err(|_| "terminal cleanup stopped")?;
            }
            return Ok(json!({}));
        }
        let action = match method {
            "process/writeStdin" => {
                let data = params.delta_base64.ok_or("terminal input is required")?;
                if data.len() > 88 * 1024 {
                    return Err("terminal input exceeds 64 KiB".into());
                }
                Action::Write(STANDARD.decode(data).map_err(|error| error.to_string())?)
            }
            "process/resizePty" => {
                let size = params.size.ok_or("terminal size is required")?;
                if size.rows == 0 || size.cols == 0 {
                    return Err("terminal size must be nonzero".into());
                }
                Action::Resize(size)
            }
            _ => return Err("unknown terminal operation".into()),
        };
        let (complete, completed) = oneshot::channel();
        input
            .send(Command { action, complete })
            .await
            .map_err(|_| "terminal has exited")?;
        completed.await.map_err(|_| "terminal has exited")??;
        Ok(json!({}))
    }
    pub(crate) fn close_session(&self, owner: SessionId) {
        for record in self
            .records
            .lock()
            .unwrap()
            .values()
            .filter(|record| record.owner == owner)
        {
            record.stop.cancel();
        }
    }
    pub(crate) fn in_use(&self, path: &Path) -> bool {
        self.records
            .lock()
            .unwrap()
            .values()
            .any(|record| record.cwd.starts_with(path))
    }
    pub(crate) async fn shutdown(&self) {
        let records: Vec<_> = self
            .records
            .lock()
            .unwrap()
            .values()
            .map(|record| {
                record.stop.cancel();
                record.finished.clone()
            })
            .collect();
        for mut finished in records {
            while !*finished.borrow_and_update() {
                if finished.changed().await.is_err() {
                    break;
                }
            }
        }
    }
}
struct Worker {
    router: SessionRouter,
    owner: SessionId,
    handle: String,
    stop: CancellationToken,
    input: mpsc::Receiver<Command>,
    ready: Option<Receipt>,
}
impl Worker {
    fn publish(&self, method: &str, mut params: Value) -> Result<(), String> {
        params["processHandle"] = self.handle.clone().into();
        self.router.send_line(
            self.owner,
            json!({"method":method,"params":params}).to_string(),
        )
    }
    async fn run(mut self, cwd: PathBuf, size: TerminalSize) {
        let mut pending: Option<(u64, Receipt)> = None;
        let result = async {
            if self.stop.is_cancelled() { return Err("terminal startup cancelled".into()); }
            let mut child = bex_process::terminal_command().and_then(|mut command| command.spawn()).map_err(|error| error.to_string())?;
            let mut stdin = child.stdin().take().ok_or("terminal input pipe unavailable")?;
            let mut output = JsonlReader::new(child.stdout().take().ok_or("terminal output pipe unavailable")?);
            let initialize = PtyCommand::Start { command:crate::platform::terminal_command().iter().map(|value| (*value).into()).collect(), cwd:cwd.to_string_lossy().into_owned(), rows:size.rows, cols:size.cols };
            let mut next_id = 0u64;
            let interaction: Result<(), String> = async {
                write(&mut stdin, &initialize).await?;
                loop {
                    tokio::select! {
                        biased;
                        _ = self.stop.cancelled() => return Ok(()),
                        line = output.read_line() => {
                            let line = line.map_err(|error| error.to_string())?.ok_or("terminal supervisor exited without a result")?;
                            match serde_json::from_str::<PtyEvent>(&line).map_err(|error| error.to_string())? {
                                PtyEvent::Started => { if let Some(ready) = self.ready.take() { let _ = ready.send(Ok(())); } }
                                PtyEvent::Output { data } => self.publish("process/outputDelta", json!({"deltaBase64":STANDARD.encode(data),"capReached":false}))?,
                                PtyEvent::Ack { id, error } => {
                                    let (expected, complete) = pending.take().ok_or("unexpected terminal acknowledgement")?;
                                    if expected != id { let _ = complete.send(Err("terminal acknowledgement ID changed".into())); return Err("terminal acknowledgement ID changed".into()); }
                                    let _ = complete.send(error.map_or(Ok(()), Err));
                                }
                                PtyEvent::Exited { code } => { self.publish("process/exited", json!({"exitCode":i32::try_from(code).unwrap_or(1)}))?; return Ok(()); }
                                PtyEvent::Failed { message } => return Err(message),
                            }
                        }
                        command = self.input.recv(), if self.ready.is_none() && pending.is_none() => {
                            let Some(command) = command else { return Ok(()) };
                            next_id = next_id.checked_add(1).ok_or("terminal operation ID exhausted")?;
                            let action = match command.action {
                                Action::Write(data) => PtyCommand::Write {id:next_id,data},
                                Action::Resize(size) => PtyCommand::Resize {id:next_id,rows:size.rows,cols:size.cols},
                            };
                            pending = Some((next_id, command.complete));
                            tokio::select! {
                                _ = self.stop.cancelled() => return Ok(()),
                                result = write(&mut stdin, &action) => result?,
                            }
                        }
                    }
                }
            }.await;
            // EOF is the supervisor's lifetime signal. Keep the owner record
            // until its process group/Job Object has finished cleanup.
            drop(output);
            drop(stdin);
            if tokio::time::timeout(std::time::Duration::from_secs(3), child.wait()).await.is_err() {
                let _ = std::pin::Pin::from(child.kill()).await;
            }
            if self.stop.is_cancelled() { let _ = self.publish("process/exited", json!({"exitCode":0})); }
            interaction
        }.await;
        if let Some(ready) = self.ready.take() {
            let _ = ready.send(Err(result
                .clone()
                .err()
                .unwrap_or_else(|| "terminal startup cancelled".into())));
        }
        if let Some((_, complete)) = pending {
            let _ = complete.send(Err("terminal has exited".into()));
        }
        if let Err(message) = result {
            let _ = self.publish("host/terminal/failed", json!({"message":message}));
        }
    }
}
async fn write(
    output: &mut tokio::process::ChildStdin,
    command: &PtyCommand,
) -> Result<(), String> {
    let mut data = serde_json::to_vec(command).map_err(|error| error.to_string())?;
    data.push(b'\n');
    output
        .write_all(&data)
        .await
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn disconnect_during_startup_releases_the_reservation_before_shutdown_returns() {
        let directory = tempfile::tempdir().unwrap();
        let terminals = Terminals::default();
        let router = SessionRouter::new();
        let connection = router.open_session(16);
        let mut starting = Box::pin(terminals.start(
            router.clone(),
            connection.id(),
            "starting".into(),
            directory.path().to_string_lossy().into_owned(),
            TerminalSize { rows: 24, cols: 80 },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                assert!(futures_util::poll!(&mut starting).is_pending());
                if terminals.records.lock().unwrap().contains_key("starting") {
                    break;
                }
                tokio::task::yield_now().await;
            }
            // On this single-thread runtime the reservation exists, but the
            // spawned worker cannot start until the next yield.
            router.close_session(connection.id());
            terminals.close_session(connection.id());
            drop(starting);
            terminals.shutdown().await;
            assert!(terminals.records.lock().unwrap().is_empty());
        })
        .await
        .unwrap();
    }
}
