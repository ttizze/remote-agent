//! Device-owned PTYs, retained across transport disconnects. The private supervisor pipe carries terminal I/O;
//! only this owner publishes events and grants access to a process handle.
mod screen;
mod session;

use crate::host_rpc::routing::{SessionId, SessionRouter};
use agent_protocol::{operations::TerminalSize, protocol::Call};
use session::Worker;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

type Receipt = oneshot::Sender<Result<(), String>>;
struct Command {
    action: Action,
    complete: Receipt,
}
enum Action {
    Write(Vec<u8>),
    Resize(TerminalSize),
    Attach(SessionId, TerminalSize),
}
struct Record {
    handle: String,
    attached: Arc<Mutex<Option<SessionId>>>,
    started: Arc<std::sync::atomic::AtomicBool>,
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
    ) -> Result<agent_protocol::models::Empty, String> {
        if handle.is_empty()
            || handle.len() > 256
            || size.rows == 0
            || size.cols == 0
            || size.rows > 250
            || size.cols > 500
        {
            return Err("invalid terminal handle or size".into());
        }
        let cwd = tokio::fs::canonicalize(cwd)
            .await
            .map_err(|error| error.to_string())?;
        let cwd = dunce::simplified(&cwd).to_owned();
        if !cwd.is_dir() {
            return Err("terminal directory is unavailable".into());
        }
        let key = format!("{}:{handle}", router.principal(owner)?);
        let existing = {
            let records = self.records.lock().unwrap();
            if let Some(record) = records.get(&key) {
                if record.cwd != cwd {
                    return Err("terminal belongs to another device or directory".into());
                }
                Some(record.input.clone())
            } else {
                None
            }
        };
        if let Some(input) = existing {
            let (complete, completed) = oneshot::channel();
            input
                .send(Command {
                    action: Action::Attach(owner, size),
                    complete,
                })
                .await
                .map_err(|_| "terminal has exited")?;
            completed.await.map_err(|_| "terminal has exited")??;
            return Ok(agent_protocol::models::Empty {});
        }
        let attached = Arc::new(Mutex::new(Some(owner)));
        let is_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (input, receiver) = mpsc::channel(32);
        let (complete, finished) = watch::channel(None);
        let stop = CancellationToken::new();
        {
            let mut records = self.records.lock().unwrap();
            router.ensure_session(owner)?;
            if records.len() >= 32 {
                return Err("terminal capacity reached".into());
            }
            if records.contains_key(&key) {
                return Err("terminal handle is already in use".into());
            }
            records.insert(
                key.clone(),
                Record {
                    handle: handle.clone(),
                    attached: attached.clone(),
                    started: is_started.clone(),
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
                attached,
                started: is_started,
                handle: handle.clone(),
                stop,
                input: receiver,
                ready: Some(ready),
            };
            let cleanup = worker.run(cwd, size).await;
            if cleanup.is_ok()
                && let Some(records) = records.upgrade()
            {
                records.lock().unwrap().remove(&key);
            }
            complete.send_replace(Some(cleanup));
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), started)
            .await
            .map_err(|_| "terminal startup timed out")?
            .map_err(|_| "terminal startup stopped")??;
        cancel_start.disarm();
        Ok(agent_protocol::models::Empty {})
    }
    pub(crate) async fn request(
        &self,
        owner: SessionId,
        call: &agent_protocol::protocol::Call,
    ) -> Result<agent_protocol::models::Empty, String> {
        let detach = matches!(call, Call::DetachTerminal(_));
        let (handle, action) = match call {
            agent_protocol::protocol::Call::WriteTerminal(params) => {
                if params.data.len() > 64 * 1024 {
                    return Err("terminal input exceeds 64 KiB".into());
                }
                (
                    params.process_handle.clone(),
                    Some(Action::Write(params.data.clone())),
                )
            }
            agent_protocol::protocol::Call::ResizeTerminal(params) => {
                if params.size.rows == 0
                    || params.size.cols == 0
                    || params.size.rows > 250
                    || params.size.cols > 500
                {
                    return Err("terminal size must be nonzero".into());
                }
                (params.handle.clone(), Some(Action::Resize(params.size)))
            }
            Call::DetachTerminal(params) => (params.handle.clone(), None),
            agent_protocol::protocol::Call::KillTerminal(params) => {
                (params.process_handle.clone(), None)
            }
            _ => return Err("unknown terminal operation".into()),
        };
        let (input, stop, mut finished) = {
            let records = self.records.lock().unwrap();
            let record = records.values().find(|record| {
                record.handle == handle && *record.attached.lock().unwrap() == Some(owner)
            });
            let Some(record) = record else {
                return if detach {
                    Ok(agent_protocol::models::Empty {})
                } else {
                    Err("terminal handle is unavailable".into())
                };
            };
            if detach {
                let mut attached = record.attached.lock().unwrap();
                if *attached == Some(owner) {
                    *attached = None;
                }
                return Ok(agent_protocol::models::Empty {});
            }
            (
                record.input.clone(),
                record.stop.clone(),
                record.finished.clone(),
            )
        };
        if matches!(call, Call::KillTerminal(_)) {
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
            return Ok(agent_protocol::models::Empty {});
        }
        let action = action.expect("kill returned above");
        let (complete, completed) = oneshot::channel();
        input
            .send(Command { action, complete })
            .await
            .map_err(|_| "terminal has exited")?;
        completed.await.map_err(|_| "terminal has exited")??;
        Ok(agent_protocol::models::Empty {})
    }
    pub(crate) fn revoke_device(&self, principal: &str) {
        let prefix = format!("{principal}:");
        for (key, record) in self.records.lock().unwrap().iter() {
            if key.starts_with(&prefix) {
                record.stop.cancel();
            }
        }
    }
    pub(crate) fn close_session(&self, owner: SessionId) {
        for record in self.records.lock().unwrap().values() {
            let mut attached = record.attached.lock().unwrap();
            if *attached == Some(owner) {
                *attached = None;
                if !record.started.load(std::sync::atomic::Ordering::Acquire) {
                    record.stop.cancel();
                }
            }
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
#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::protocol::Notification;
    #[cfg(unix)]
    #[tokio::test]
    async fn reattach_retains_shell_and_detach_only_releases_one_terminal() {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let directory = tempfile::tempdir().unwrap();
            let cwd = directory.path().to_string_lossy().into_owned();
            let size = TerminalSize { cols: 80, rows: 24 };
            let router = SessionRouter::new();
            let terminals = Terminals::default();
            let first = router.open_authenticated_session(Some("phone".into()));
            for handle in ["one", "two"] {
                terminals.start(router.clone(), first.id(), handle.into(), cwd.clone(), size).await.unwrap();
            }
            terminals.request(first.id(), &Call::WriteTerminal(agent_protocol::operations::TerminalWrite { process_handle: "one".into(), data: "BEX_RETAINED=survived\n".as_bytes().to_vec() })).await.unwrap();
            terminals.request(first.id(), &Call::DetachTerminal(agent_protocol::operations::DetachTerminal { handle: "one".into() })).await.unwrap();
            assert!(terminals.request(first.id(), &Call::WriteTerminal(agent_protocol::operations::TerminalWrite { process_handle: "one".into(), data: b"a".to_vec() })).await.is_err());
            terminals.request(first.id(), &Call::WriteTerminal(agent_protocol::operations::TerminalWrite { process_handle: "two".into(), data: "true\n".as_bytes().to_vec() })).await.unwrap();
            router.close_session(first.id()); terminals.close_session(first.id());
            let mut second = router.open_authenticated_session(Some("phone".into()));
            terminals.start(router.clone(), second.id(), "one".into(), cwd.clone(), size).await.unwrap();
            let mut restored = false;
            while let Some(line) = second.recv().await {
                if matches!(agent_protocol::protocol::decode::<Notification>(&line).unwrap(), Notification::TerminalRestored { .. }) { restored=true; break; }
            }
            assert!(restored);
            terminals.request(second.id(), &Call::WriteTerminal(agent_protocol::operations::TerminalWrite { process_handle: "one".into(), data: "printf '%s' \"$BEX_RETAINED\" > retained\n".as_bytes().to_vec() })).await.unwrap();
            loop {
                if std::fs::read_to_string(directory.path().join("retained")).ok().as_deref()==Some("survived") {break;}
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            terminals.request(second.id(), &Call::WriteTerminal(agent_protocol::operations::TerminalWrite { process_handle: "one".into(), data: "stty -echo -icanon min 0 time 5; printf '\\033[6n'; dd bs=64 count=1 of=query-reply 2>/dev/null; stty sane\n".as_bytes().to_vec() })).await.unwrap();
            loop {
                if let Ok(bytes)=std::fs::read(directory.path().join("query-reply"))
                    && bytes.starts_with(b"\x1b[") && bytes.ends_with(b"R") {break;}
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            let stranger=router.open_authenticated_session(Some("other-phone".into()));
            assert!(terminals.request(stranger.id(),&Call::KillTerminal(agent_protocol::operations::TerminalKill { process_handle: "one".into() })).await.is_err());
            terminals.start(router.clone(),stranger.id(),"one".into(),cwd,size).await.unwrap();
            assert_eq!(terminals.records.lock().unwrap().len(),3);
            terminals.shutdown().await;
        }).await.expect("reattach stalled");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn kill_and_disconnect_keep_ownership_until_shell_job_groups_are_gone() {
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            for disconnect in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let cwd = dunce::canonicalize(directory.path()).unwrap();
                let terminals = Terminals::default();
                let router = SessionRouter::new();
                let connection = router.open_session();
                terminals.start(router, connection.id(), "jobs".into(), directory.path().to_string_lossy().into_owned(), TerminalSize {rows:24, cols:80}).await.unwrap();
                // Linux validation runs this Host with SHELL=/bin/sh (dash).
                // Disable interactive history expansion for Bash and Zsh on macOS.
                let command = "[ -z \"${BASH_VERSION-}\" ] || set +H\n[ -z \"${ZSH_VERSION-}\" ] || unsetopt BANG_HIST\nsleep 120 & first=$!; sleep 120 & printf '%s %s %s\\n' \"$$\" \"$first\" \"$!\" > owned-pids; wait\n";
                terminals.request(connection.id(), &agent_protocol::protocol::Call::WriteTerminal(agent_protocol::operations::TerminalWrite { process_handle: "jobs".into(), data: command.as_bytes().to_vec() })).await.unwrap();
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
                    let call = agent_protocol::protocol::Call::KillTerminal(agent_protocol::operations::TerminalKill { process_handle: "jobs".into() });
                    let mut kill = Box::pin(terminals.request(connection.id(), &call));
                    assert!(futures_util::poll!(&mut kill).is_pending());
                    assert!(terminals.in_use(&cwd));
                    kill.await.unwrap();
                }
                assert!(!terminals.in_use(&cwd));
                for pid in pids.split_whitespace() {
                    loop {
                        let output = std::process::Command::new("ps").args(["-o","stat=","-p",pid]).output().unwrap();
                        let state = String::from_utf8_lossy(&output.stdout);
                        if state.trim().is_empty() || state.trim().starts_with('Z') { break; }
                        // macOS can report an exiting process as "?E" before it disappears.
                        assert!(state.contains('E'), "process {pid} survived cleanup: {state}");
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                }
            }
        }).await.expect("terminal cleanup stalled");
    }

    #[tokio::test]
    async fn disconnect_during_startup_releases_the_reservation_before_shutdown_returns() {
        let directory = tempfile::tempdir().unwrap();
        let terminals = Terminals::default();
        let router = SessionRouter::new();
        let connection = router.open_session();
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
                if terminals
                    .records
                    .lock()
                    .unwrap()
                    .values()
                    .any(|record| record.handle == "starting")
                {
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
