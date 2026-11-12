//! One terminal's supervisor protocol, device attachment, and process lifetime.
//! Cleanup completes before the registry is allowed to release ownership.
use super::{Action, Command, Launch, Receipt, screen::TerminalScreen};
use crate::host_rpc::routing::{SessionId, SessionRouter};
use agent_process::{PtyCommand, PtyEvent};
use agent_protocol::{operations::TerminalSize, protocol::Notification};
use agent_transport::peer::JsonlReader;
use std::sync::{Arc, Mutex};
use tokio::{io::AsyncWriteExt, sync::mpsc};
use tokio_util::sync::CancellationToken;

pub(super) struct Worker {
    pub(super) router: SessionRouter,
    pub(super) attached: Arc<Mutex<Option<SessionId>>>,
    pub(super) started: Arc<std::sync::atomic::AtomicBool>,
    pub(super) handle: String,
    pub(super) stop: CancellationToken,
    pub(super) input: mpsc::Receiver<Command>,
    pub(super) ready: Option<Receipt>,
    pub(super) observer: Option<super::Observer>,
    pub(super) replay: Option<Arc<Mutex<super::Replay>>>,
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
    pub(super) async fn run(mut self, launch: Launch, size: TerminalSize) -> Result<(), String> {
        let mut pending: Option<(u64, Receipt)> = None;
        let mut screen = TerminalScreen::new(size);
        let mut cleanup = Ok(());
        let result = async {
            if self.stop.is_cancelled() { return Err("terminal startup cancelled".into()); }
            let mut command = agent_process::terminal_command().map_err(|error| error.to_string())?;
            for (key, value) in &launch.environment {
                match value { Some(value) => {command.command_mut().env(key, value);}, None => {command.command_mut().env_remove(key);} }
            }
            let mut child = command.spawn().map_err(|error| error.to_string())?;
            cleanup = Err("terminal cleanup incomplete".into());
            let mut stdin = child.stdin().take().ok_or("terminal input pipe unavailable")?;
            let mut output = JsonlReader::new(child.stdout().take().ok_or("terminal output pipe unavailable")?);
            let initialize = PtyCommand::Start { command:launch.arguments, cwd:launch.directory.to_string_lossy().into_owned(), rows:size.rows, cols:size.cols };
            let mut next_id = 0u64;
            let interaction: Result<(), String> = async {
                write(&mut stdin, &initialize).await?;
                loop {
                    tokio::select! {
                        biased;
                        _ = self.stop.cancelled() => return Ok(()),
                        line = output.read_line() => {
                            let line = line.map_err(|error| error.to_string())?.ok_or("terminal supervisor exited without a result")?;
                            let event = serde_json::from_str::<PtyEvent>(&line).map_err(|error| error.to_string())?;
                            if let PtyEvent::Exited { code } = &event && let Some(replay) = &self.replay {
                                *replay.lock().unwrap() = super::Replay { checkpoint: Some(screen.checkpoint()), code: Some(i32::try_from(*code).unwrap_or(1)) };
                            }
                            if let Some(observer) = &self.observer { observer(&event); }
                            match event {
                                PtyEvent::Started => { let checkpoint = screen.checkpoint(); self.publish(Notification::TerminalRestored { handle: self.handle.clone(), data: checkpoint.data, cols: checkpoint.size.cols, rows: checkpoint.size.rows }); self.started.store(true, std::sync::atomic::Ordering::Release); if let Some(ready) = self.ready.take() { let _ = ready.send(Ok(())); } }
                                PtyEvent::Output { data } => {
                                    // The Host is the sole terminal-query responder, even during disconnects.
                                    for reply in screen.feed(&data) {
                                        write(&mut stdin, &PtyCommand::Write { id: 0, data: reply }).await?;
                                    }
                                    self.publish(Notification::Output { handle: self.handle.clone(), data });
                                },
                                PtyEvent::Ack { id, error } => {
                                    if id == 0 { if let Some(error)=error {return Err(error);} continue; }
                                    let (expected, complete) = pending.take().ok_or("unexpected terminal acknowledgement")?;
                                    if expected != id { let _ = complete.send(Err("terminal acknowledgement ID changed".into())); return Err("terminal acknowledgement ID changed".into()); }
                                    let _ = complete.send(error.map_or(Ok(()), Err));
                                }
                                PtyEvent::Exited { code } => {
                                    let code = i32::try_from(code).unwrap_or(1);
                                    self.publish(Notification::Exited { handle: self.handle.clone(), code }); return Ok(());
                                }
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
                                if screen.size() != size {
                                    screen.resize(size);
                                    write(&mut stdin,&PtyCommand::Resize{id:0,rows:size.rows,cols:size.cols}).await?;
                                }
                                let checkpoint = screen.checkpoint();
                                let result=self.router.send(owner, Notification::TerminalRestored { handle: self.handle.clone(), data: checkpoint.data, cols: checkpoint.size.cols, rows: checkpoint.size.rows });
                                if result.is_err() { *self.attached.lock().unwrap()=None; }
                                let _=command.complete.send(result);
                                continue;
                            }
                            next_id = next_id.checked_add(1).ok_or("terminal operation ID exhausted")?;
                            let action = match command.action {
                                Action::Write(data) => PtyCommand::Write {id:next_id,data},
                                Action::Resize(size) => {screen.resize(size); PtyCommand::Resize {id:next_id,rows:size.rows,cols:size.cols}},
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
            if self.stop.is_cancelled() {
                if let Some(replay) = &self.replay { *replay.lock().unwrap() = super::Replay { checkpoint: Some(screen.checkpoint()), code: Some(0) }; }
                self.publish(Notification::Exited { handle: self.handle.clone(), code: 0 });
            }
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
            if let Some(replay) = &self.replay {
                *replay.lock().unwrap() = super::Replay {
                    checkpoint: Some(screen.checkpoint()),
                    code: Some(1),
                };
            }
            if let Some(observer) = &self.observer {
                observer(&PtyEvent::Failed {
                    message: message.clone(),
                });
            }
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
