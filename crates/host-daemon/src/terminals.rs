//! Connection-owned PTYs. The private supervisor pipe carries terminal I/O;
//! only this owner publishes events and grants access to a process handle.
use crate::host_rpc::routing::{SessionId, SessionRouter};
use agent_core::{client::TerminalSize, peer::JsonlReader};
use bex_process::{PtyCommand, PtyEvent};
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
    finished: watch::Receiver<Option<Result<(), String>>>,
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
    ) -> Result<agent_core::models::Empty, String> {
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
        let (complete, finished) = watch::channel(None);
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
            let worker = Worker {
                router,
                owner,
                handle: handle.clone(),
                stop,
                input: receiver,
                ready: Some(ready),
            };
            let cleanup = worker.run(cwd, size).await;
            if cleanup.is_ok()
                && let Some(records) = records.upgrade()
            {
                records.lock().unwrap().remove(&handle);
            }
            complete.send_replace(Some(cleanup));
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), started)
            .await
            .map_err(|_| "terminal startup timed out")?
            .map_err(|_| "terminal startup stopped")??;
        cancel_start.disarm();
        Ok(agent_core::models::Empty {})
    }
    pub(crate) async fn request(
        &self,
        owner: SessionId,
        call: &agent_core::protocol::Call,
    ) -> Result<agent_core::models::Empty, String> {
        let method = call.method();
        let (handle, action) = match call {
            agent_core::protocol::Call::WriteTerminal(params) => {
                if params.data.len() > 64 * 1024 {
                    return Err("terminal input exceeds 64 KiB".into());
                }
                (
                    params.process_handle.clone(),
                    Some(Action::Write(params.data.clone())),
                )
            }
            agent_core::protocol::Call::ResizeTerminal(params) => {
                if params.size.rows == 0 || params.size.cols == 0 {
                    return Err("terminal size must be nonzero".into());
                }
                (params.handle.clone(), Some(Action::Resize(params.size)))
            }
            agent_core::protocol::Call::KillTerminal(params) => {
                (params.process_handle.clone(), None)
            }
            _ => return Err("unknown terminal operation".into()),
        };
        let (input, stop, mut finished) = {
            let records = self.records.lock().unwrap();
            let record = records
                .get(&handle)
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
            loop {
                if let Some(result) = finished.borrow_and_update().clone() {
                    result?;
                    break;
                }
                finished
                    .changed()
                    .await
                    .map_err(|_| "terminal cleanup stopped")?;
            }
            return Ok(agent_core::models::Empty {});
        }
        let action = action.expect("kill returned above");
        let (complete, completed) = oneshot::channel();
        input
            .send(Command { action, complete })
            .await
            .map_err(|_| "terminal has exited")?;
        completed.await.map_err(|_| "terminal has exited")??;
        Ok(agent_core::models::Empty {})
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
            while finished.borrow_and_update().is_none() {
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
    async fn run(mut self, cwd: PathBuf, size: TerminalSize) -> Result<(), String> {
        let mut pending: Option<(u64, Receipt)> = None;
        let mut cleanup = Ok(());
        let result = async {
            if self.stop.is_cancelled() { return Err("terminal startup cancelled".into()); }
            let mut child = bex_process::terminal_command().and_then(|mut command| command.spawn()).map_err(|error| error.to_string())?;
            cleanup = Err("terminal cleanup incomplete".into());
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
                                PtyEvent::Output { data } => self.router.send(self.owner, agent_core::protocol::Notification::Output { handle: self.handle.clone(), data, cap_reached: false })?,
                                PtyEvent::Ack { id, error } => {
                                    let (expected, complete) = pending.take().ok_or("unexpected terminal acknowledgement")?;
                                    if expected != id { let _ = complete.send(Err("terminal acknowledgement ID changed".into())); return Err("terminal acknowledgement ID changed".into()); }
                                    let _ = complete.send(error.map_or(Ok(()), Err));
                                }
                                PtyEvent::Exited { code } => { self.router.send(self.owner, agent_core::protocol::Notification::Exited { handle: self.handle.clone(), code: i32::try_from(code).unwrap_or(1) })?; return Ok(()); }
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
            // until its entire PTY session/Job Object has finished cleanup.
            // Killing the supervisor on a deadline would strand its jobs.
            drop(output);
            drop(stdin);
            cleanup = child.wait().await.map_err(|error| error.to_string()).and_then(|status| {
                if status.success() { Ok(()) } else { Err(format!("terminal cleanup failed: {status}")) }
            });
            cleanup.clone()?;
            if self.stop.is_cancelled() { let _ = self.router.send(self.owner, agent_core::protocol::Notification::Exited { handle: self.handle.clone(), code: 0 }); }
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
            let _ = self.router.send(
                self.owner,
                agent_core::protocol::Notification::TerminalFailed {
                    handle: self.handle.clone(),
                    reason: message,
                },
            );
        }
        cleanup
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
    #[cfg(unix)]
    #[tokio::test]
    async fn kill_and_disconnect_keep_ownership_until_shell_job_groups_are_gone() {
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            for disconnect in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let cwd = std::fs::canonicalize(directory.path()).unwrap();
                let terminals = Terminals::default();
                let router = SessionRouter::new();
                let connection = router.open_session(64);
                terminals.start(router, connection.id(), "jobs".into(), directory.path().to_string_lossy().into_owned(), TerminalSize {rows:24, cols:80}).await.unwrap();
                // Linux validation runs this Host with SHELL=/bin/sh (dash).
                // Disable interactive history expansion for Bash on macOS.
                let command = "[ -z \"${BASH_VERSION-}\" ] || set +H\nsleep 120 & first=$!; sleep 120 & printf '%s %s %s\\n' \"$$\" \"$first\" \"$!\" > owned-pids; wait\n";
                terminals.request(connection.id(), &agent_core::protocol::Call::WriteTerminal(agent_core::client::TerminalWrite { process_handle: "jobs".into(), data: command.as_bytes().to_vec() })).await.unwrap();
                let pids = loop {
                    if let Ok(text) = std::fs::read_to_string(directory.path().join("owned-pids"))
                        && text.split_whitespace().count() == 3
                    { break text; }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                };
                let mut groups = pids.split_whitespace().map(|pid| {
                    let output = std::process::Command::new("ps").args(["-o","pgid=","-p",pid]).output().unwrap();
                    String::from_utf8(output.stdout).unwrap().trim().to_owned()
                });
                let shell_group = groups.next().unwrap();
                for group in groups {
                    assert!(!group.is_empty());
                    assert_ne!(shell_group, group, "fixture must create job-control groups");
                }
                assert!(terminals.in_use(&cwd));
                if disconnect {
                    terminals.close_session(connection.id());
                    assert!(terminals.in_use(&cwd));
                    terminals.shutdown().await;
                } else {
                    let call = agent_core::protocol::Call::KillTerminal(agent_core::client::TerminalKill { process_handle: "jobs".into() });
                    let mut kill = Box::pin(terminals.request(connection.id(), &call));
                    assert!(futures_util::poll!(&mut kill).is_pending());
                    assert!(terminals.in_use(&cwd));
                    kill.await.unwrap();
                }
                assert!(!terminals.in_use(&cwd));
                for pid in pids.split_whitespace() {
                    let output = std::process::Command::new("ps").args(["-o","stat=","-p",pid]).output().unwrap();
                    let state = String::from_utf8_lossy(&output.stdout);
                    assert!(state.trim().is_empty() || state.trim().starts_with('Z'), "process {pid} survived cleanup: {state}");
                }
            }
        }).await.expect("terminal cleanup stalled");
    }

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
