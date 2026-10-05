use bex_process::{PtyCommand, PtyEvent};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{self, Read, Write};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[cfg(unix)]
#[path = "pty_session.rs"]
mod session;

struct OwnedPty {
    #[cfg(not(unix))]
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    #[cfg(unix)]
    pid: Option<u32>,
}
impl Drop for OwnedPty {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}
impl OwnedPty {
    fn cleanup(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            session::terminate(pid as libc::pid_t)?;
            self.pid = None;
        }
        #[cfg(not(unix))]
        // The Host's Job Object also owns the descendants. The shell may
        // already have exited naturally before we reach this point.
        let _ = self.killer.kill();
        Ok(())
    }
}

async fn emit(output: &mut tokio::io::Stdout, event: PtyEvent) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(&event)?;
    bytes.push(b'\n');
    output.write_all(&bytes).await?;
    output.flush().await
}
fn io_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}
fn size(rows: u16, cols: u16) -> io::Result<PtySize> {
    if rows == 0 || cols == 0 {
        return Err(io_error("terminal size must be nonzero"));
    }
    Ok(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })
}

pub async fn run() -> io::Result<i32> {
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    let mut output = tokio::io::stdout();
    let line = input
        .next_line()
        .await?
        .ok_or_else(|| io_error("terminal initialization missing"))?;
    let PtyCommand::Start {
        command,
        cwd,
        rows,
        cols,
    } = serde_json::from_str(&line)?
    else {
        return Err(io_error("expected terminal initialization"));
    };
    let program = command
        .first()
        .ok_or_else(|| io_error("terminal command missing"))?;
    let pair = native_pty_system()
        .openpty(size(rows, cols)?)
        .map_err(io_error)?;
    let mut command_builder = CommandBuilder::new(program);
    command_builder.args(&command[1..]);
    command_builder.cwd(cwd);
    command_builder.env("TERM", "xterm-256color");
    command_builder.env("COLORTERM", "truecolor");
    let mut child = pair
        .slave
        .spawn_command(command_builder)
        .map_err(io_error)?;
    let mut owned = OwnedPty {
        #[cfg(not(unix))]
        killer: child.clone_killer(),
        #[cfg(unix)]
        pid: child.process_id(),
    };
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().map_err(io_error)?;
    let mut writer = pair.master.take_writer().map_err(io_error)?;
    let (send, mut received) = tokio::sync::mpsc::channel(16);
    // The Host owns at most one user write and one coalesced query reply.
    // Both can arrive before the writer thread has consumed either message.
    let (write_input, write_commands) = std::sync::mpsc::sync_channel::<(u64, Vec<u8>)>(2);
    let written = send.clone();
    std::thread::spawn(move || {
        while let Ok((id, data)) = write_commands.recv() {
            let error = writer.write_all(&data).err().map(|error| error.to_string());
            if written.blocking_send(PtyEvent::Ack { id, error }).is_err() {
                break;
            }
        }
    });
    std::thread::spawn(move || {
        let mut bytes = [0; 8192];
        loop {
            match reader.read(&mut bytes) {
                Ok(0) | Err(_) => break,
                Ok(length) => {
                    if send
                        .blocking_send(PtyEvent::Output {
                            data: bytes[..length].to_vec(),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
    // Keep the session leader unreaped until all its jobs are gone. Its PID
    // must not be reused while we identify processes by their session ID.
    #[cfg(unix)]
    let mut exited = {
        let pid = child
            .process_id()
            .ok_or_else(|| io_error("PTY PID missing"))?;
        tokio::task::spawn_blocking(move || session::wait_without_reaping(pid))
    };
    #[cfg(not(unix))]
    let mut exited = tokio::task::spawn_blocking(move || child.wait());
    let result: io::Result<u32> = async {
        emit(&mut output, PtyEvent::Started).await?;
        let mut reading = true;
        loop {
            tokio::select! {
                bytes = received.recv(), if reading => match bytes {
                    Some(event) => emit(&mut output, event).await?,
                    None => reading = false,
                },
                status = &mut exited => return status.map_err(io_error)?.map(|status| status.exit_code()),
                line = input.next_line() => {
                    let Some(line) = line? else { return Ok(0) };
                    let command: PtyCommand = serde_json::from_str(&line)?;
                    let (id, result) = match command {
                        PtyCommand::Write { id, data } => {
                            match write_input.try_send((id, data)) {
                                Ok(()) => continue,
                                Err(error) => (id, Err(io_error(error))),
                            }
                        },
                        PtyCommand::Resize { id, rows, cols } => (id, size(rows, cols).and_then(|size| pair.master.resize(size).map_err(io_error))),
                        PtyCommand::Start { .. } => return Err(io_error("terminal already started")),
                    };
                    emit(&mut output, PtyEvent::Ack { id, error: result.err().map(|error| error.to_string()) }).await?;
                }
            }
        }
    }.await;
    owned.cleanup()?;
    drop(write_input);
    drop(pair.master);
    if !exited.is_finished() {
        let _ = (&mut exited).await;
    }
    #[cfg(unix)]
    child.wait()?;
    // The child can exit before the final PTY read arrives. Drain those bytes
    // before publishing its exit, with a bound for descendants holding handles.
    let drain = async {
        while let Some(event) = received.recv().await {
            emit(&mut output, event).await?;
        }
        Ok::<_, io::Error>(())
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), drain).await;
    match result {
        Ok(code) => {
            // The Host closes the output pipe when cancelling. Cleanup has
            // succeeded even if the final event cannot reach that connection.
            let _ = emit(&mut output, PtyEvent::Exited { code }).await;
            Ok(0)
        }
        Err(error) => {
            let _ = emit(
                &mut output,
                PtyEvent::Failed {
                    message: error.to_string(),
                },
            )
            .await;
            Ok(0)
        }
    }
}
