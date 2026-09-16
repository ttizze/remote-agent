use bex_process::{PtyCommand, PtyEvent};
use portable_pty::{ChildKiller, CommandBuilder, PtySize, native_pty_system};
use std::io::{self, Read, Write};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct OwnedPty {
    killer: Box<dyn ChildKiller + Send + Sync>,
    #[cfg(unix)]
    pid: Option<u32>,
}
impl Drop for OwnedPty {
    fn drop(&mut self) {
        // Let the interactive shell propagate hangup to its jobs, then ensure
        // the owned process group cannot survive the supervisor.
        let _ = self.killer.kill();
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            unsafe {
                libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
            }
        }
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
    let owned = OwnedPty {
        killer: child.clone_killer(),
        #[cfg(unix)]
        pid: child.process_id(),
    };
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().map_err(io_error)?;
    let mut writer = pair.master.take_writer().map_err(io_error)?;
    let (send, mut received) = tokio::sync::mpsc::channel(16);
    let (write_input, write_commands) = std::sync::mpsc::sync_channel::<(u64, Vec<u8>)>(1);
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
    let _ = owned.killer.clone_killer().kill();
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    drop(owned);
    drop(write_input);
    drop(pair.master);
    if !exited.is_finished() {
        let _ = (&mut exited).await;
    }
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
            emit(&mut output, PtyEvent::Exited { code }).await?;
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
            Err(error)
        }
    }
}
