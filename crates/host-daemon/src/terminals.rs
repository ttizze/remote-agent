//! Device-owned PTYs, retained across transport disconnects. The private supervisor pipe carries terminal I/O;
//! only this owner publishes events and grants access to a process handle.
use crate::host_rpc::routing::{SessionId, SessionRouter};
use agent_protocol::{
    operations::TerminalSize,
    protocol::{Call, Notification},
};
use agent_transport::peer::JsonlReader;
use alacritty_terminal::grid::Dimensions as _;
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

#[derive(Clone, Default)]
struct Replies(Arc<Mutex<Vec<alacritty_terminal::event::Event>>>);
impl alacritty_terminal::event::EventListener for Replies {
    fn send_event(&self, event: alacritty_terminal::event::Event) {
        use alacritty_terminal::event::Event;
        if matches!(
            event,
            Event::PtyWrite(_)
                | Event::ColorRequest(..)
                | Event::TextAreaSizeRequest(_)
                | Event::ClipboardLoad(..)
        ) {
            self.0.lock().unwrap().push(event);
        }
    }
}
struct Dimensions(TerminalSize);
impl alacritty_terminal::grid::Dimensions for Dimensions {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }
    fn screen_lines(&self) -> usize {
        self.0.rows as usize
    }
    fn columns(&self) -> usize {
        self.0.cols as usize
    }
}
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
struct Worker {
    router: SessionRouter,
    attached: Arc<Mutex<Option<SessionId>>>,
    started: Arc<std::sync::atomic::AtomicBool>,
    handle: String,
    stop: CancellationToken,
    input: mpsc::Receiver<Command>,
    ready: Option<Receipt>,
}
impl Worker {
    fn publish(&self, event: Notification) {
        let mut attached = self.attached.lock().unwrap();
        if let Some(owner) = *attached
            && self.router.send(owner, event).is_err()
        {
            *attached = None;
        }
    }
    async fn run(mut self, cwd: PathBuf, size: TerminalSize) -> Result<(), String> {
        let mut pending: Option<(u64, Receipt)> = None;
        let replies = Replies::default();
        let mut screen = alacritty_terminal::Term::new(
            alacritty_terminal::term::Config::default(),
            &Dimensions(size),
            replies.clone(),
        );
        let mut parser: alacritty_terminal::vte::ansi::Processor = Default::default();
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
                                PtyEvent::Started => { self.publish(Notification::TerminalRestored { handle: self.handle.clone(), data: screen.ansi_checkpoint(None), cols: size.cols, rows: size.rows }); self.started.store(true, std::sync::atomic::Ordering::Release); if let Some(ready) = self.ready.take() { let _ = ready.send(Ok(())); } }
                                PtyEvent::Output { data } => {
                                    parser.advance(&mut screen, &data);
                                    // The Host is the sole terminal-query responder, even during disconnects.
                                    let output_replies=std::mem::take(&mut *replies.0.lock().unwrap());
                                    for reply in output_replies {
                                        use alacritty_terminal::event::{Event, WindowSize};
                                        use alacritty_terminal::vte::ansi::Rgb;
                                        let data = match reply {
                                            Event::PtyWrite(data)=>data,
                                            Event::ColorRequest(index,format)=> {
                                                let value=agent_protocol::operations::terminal_color(index as u16);
                                                let color=screen.colors()[index].unwrap_or(Rgb {r:(value>>16) as u8,g:(value>>8) as u8,b:value as u8});
                                                format(color)
                                            }
                                            Event::TextAreaSizeRequest(format)=>format(WindowSize {num_cols:screen.columns() as u16,num_lines:screen.screen_lines() as u16,cell_width:0,cell_height:0}),
                                            Event::ClipboardLoad(_,format)=>format(""),
                                            _=>unreachable!(),
                                        };
                                        write(&mut stdin,&PtyCommand::Write{id:0,data:data.into_bytes()}).await?;
                                    }
                                    self.publish(Notification::Output { handle: self.handle.clone(), data });
                                },
                                PtyEvent::Ack { id, error } => {
                                    if id == 0 { if let Some(error)=error {return Err(error);} continue; }
                                    let (expected, complete) = pending.take().ok_or("unexpected terminal acknowledgement")?;
                                    if expected != id { let _ = complete.send(Err("terminal acknowledgement ID changed".into())); return Err("terminal acknowledgement ID changed".into()); }
                                    let _ = complete.send(error.map_or(Ok(()), Err));
                                }
                                PtyEvent::Exited { code } => { self.publish(Notification::Exited { handle: self.handle.clone(), code: i32::try_from(code).unwrap_or(1) }); return Ok(()); }
                                PtyEvent::Failed { message } => return Err(message),
                            }
                        }
                        command = self.input.recv(), if self.ready.is_none() && pending.is_none() => {
                            let Some(command) = command else { return Ok(()) };
                            if let Action::Attach(owner, size) = command.action {
                                if let Err(error) = self.router.ensure_session(owner) { let _=command.complete.send(Err(error)); continue; }
                                let previous=self.attached.lock().unwrap().replace(owner);
                                if let Some(previous)=previous.filter(|previous|*previous!=owner) {
                                    let _=self.router.send(previous, Notification::TerminalDetached { handle: self.handle.clone() });
                                }
                                if screen.columns()!=usize::from(size.cols) || screen.screen_lines()!=usize::from(size.rows) {
                                    screen.resize(Dimensions(size));
                                    write(&mut stdin,&PtyCommand::Resize{id:0,rows:size.rows,cols:size.cols}).await?;
                                }
                                let mut data=screen.ansi_checkpoint(parser.preceding_char());
                                data.extend(parser.checkpoint_tail());
                                let result=self.router.send(owner, Notification::TerminalRestored { handle: self.handle.clone(), data, cols: screen.columns() as u16, rows: screen.screen_lines() as u16 });
                                if result.is_err() { *self.attached.lock().unwrap()=None; }
                                let _=command.complete.send(result);
                                continue;
                            }
                            next_id = next_id.checked_add(1).ok_or("terminal operation ID exhausted")?;
                            let action = match command.action {
                                Action::Write(data) => PtyCommand::Write {id:next_id,data},
                                Action::Resize(size) => {screen.resize(Dimensions(size)); PtyCommand::Resize {id:next_id,rows:size.rows,cols:size.cols}},
                                Action::Attach(_, _) => unreachable!(),
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
            if self.stop.is_cancelled() { self.publish(Notification::Exited { handle: self.handle.clone(), code: 0 }); }
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
            self.publish(Notification::TerminalFailed {
                handle: self.handle.clone(),
                reason: message,
            });
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
    #[test]
    fn checkpoint_restores_screen_modes_and_split_sequences() {
        use alacritty_terminal::{Term, term::Config, vte::ansi::Processor};
        let size = TerminalSize { rows: 5, cols: 12 };
        let basic = [
            (
                b"hello\r\nworld\x1b[31m!\x1b[3;8H".as_slice(),
                b"again".as_slice(),
            ),
            (
                b"original\x1b[?1049h\x1b[2;4r\x1b[?6hALT\x1b[?2004h",
                b"\r\nmore\x1b[?1049l!",
            ),
            (b"012345678901", b"next"),
            (b"hi\x1b[38;2;12;", b"34;56mcolor"),
            (b"\xe6\x97", b"\xa5\xe6\x9c\xac"),
            (b"abc\x1b[2J", b"\x1b[3b"),
            (b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix", b"\r\nseven"),
        ];
        let sequences = [
            "日本語\r\n12345678901日\r\ne\u{301}\x1b[31;44;1mred\x1b[0m",
            "abc\x1b7\r\nother\x1b8!",
            "123456789012\x1b7\r\nnext\x1b8X",
            "screen\x1b[?1049h\x1b[2;4r\x1b[?6hALT\x1b[?1049l!",
            "abc\x1b]0;title\x1b\\hello\x1b[38;2;12;34;56mRGB",
            "before\x1b[?2026hupdate\r\nmore\x1b[?2026lafter",
        ];
        let cases =
            basic.into_iter().chain(sequences.iter().flat_map(|text| {
                (0..=text.len()).map(move |index| text.as_bytes().split_at(index))
            }));
        for (before, after) in cases {
            let mut original = Term::new(Config::default(), &Dimensions(size), Replies::default());
            let mut parser: Processor = Default::default();
            parser.advance(&mut original, before);
            let mut restored = Term::new(Config::default(), &Dimensions(size), Replies::default());
            let mut reader: Processor = Default::default();
            let mut checkpoint = original.ansi_checkpoint(parser.preceding_char());
            checkpoint.extend(parser.checkpoint_tail());
            reader.advance(&mut restored, &checkpoint);
            parser.advance(&mut original, after);
            reader.advance(&mut restored, after);
            assert_eq!(original.mode(), restored.mode(), "{before:?}");
            assert_eq!(
                original.grid().cursor.point,
                restored.grid().cursor.point,
                "{before:?}"
            );
            assert_eq!(
                original.grid().history_size(),
                restored.grid().history_size(),
                "{before:?}"
            );
            for row in -(original.grid().history_size() as i32)..5 {
                for col in 0..12 {
                    use alacritty_terminal::index::{Column, Line};
                    let a = &original.grid()[Line(row)][Column(col)];
                    let b = &restored.grid()[Line(row)][Column(col)];
                    assert_eq!(
                        (a.c, a.fg, a.bg, a.flags, a.zerowidth()),
                        (b.c, b.fg, b.bg, b.flags, b.zerowidth()),
                        "{before:?}, {row}:{col}"
                    );
                }
            }
        }
    }

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
                let cwd = std::fs::canonicalize(directory.path()).unwrap();
                let terminals = Terminals::default();
                let router = SessionRouter::new();
                let connection = router.open_session();
                terminals.start(router, connection.id(), "jobs".into(), directory.path().to_string_lossy().into_owned(), TerminalSize {rows:24, cols:80}).await.unwrap();
                // Linux validation runs this Host with SHELL=/bin/sh (dash).
                // Disable interactive history expansion for Bash on macOS.
                let command = "[ -z \"${BASH_VERSION-}\" ] || set +H\nsleep 120 & first=$!; sleep 120 & printf '%s %s %s\\n' \"$$\" \"$first\" \"$!\" > owned-pids; wait\n";
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
