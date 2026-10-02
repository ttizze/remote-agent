use alacritty_terminal::{
    Term,
    event::{Event, EventListener, WindowSize},
    event_loop::{EventLoop, Msg},
    grid::Dimensions,
    sync::FairMutex,
    term::Config,
    tty::{self, Options, Shell},
    vte::ansi::Rgb,
};
use bex_process::{PtyCommand, PtyEvent};
use std::{
    io,
    sync::{Arc, Mutex},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[cfg(unix)]
#[path = "pty_session.rs"]
mod session;

#[derive(Clone)]
struct Events {
    output: tokio::sync::mpsc::Sender<Event>,
    replies: Arc<Mutex<Vec<Event>>>,
}
impl EventListener for Events {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(_)
            | Event::ColorRequest(..)
            | Event::TextAreaSizeRequest(_)
            | Event::ClipboardLoad(..) => {
                self.replies.lock().unwrap().push(event);
            }
            Event::PtyOutput(_)
            | Event::Checkpoint { .. }
            | Event::OperationComplete { .. }
            | Event::PtyError(_)
            | Event::ChildExit(_)
            | Event::Exit => {
                let _ = self.output.blocking_send(event);
            }
            _ => {}
        }
    }
    fn take_replies(&self, term: &Term<Self>) -> Vec<u8> {
        std::mem::take(&mut *self.replies.lock().unwrap())
            .into_iter()
            .flat_map(|event| {
                match event {
                    Event::PtyWrite(data) => data,
                    Event::ColorRequest(index, format) => {
                        let value = agent_protocol::operations::terminal_color(index as u16);
                        format(term.colors()[index].unwrap_or(Rgb {
                            r: (value >> 16) as u8,
                            g: (value >> 8) as u8,
                            b: value as u8,
                        }))
                    }
                    Event::TextAreaSizeRequest(format) => format(WindowSize {
                        num_lines: term.screen_lines() as u16,
                        num_cols: term.columns() as u16,
                        cell_width: 0,
                        cell_height: 0,
                    }),
                    Event::ClipboardLoad(_, format) => format(""),
                    _ => unreachable!(),
                }
                .into_bytes()
            })
            .collect()
    }
}
async fn emit(output: &mut tokio::io::Stdout, event: PtyEvent) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(&event)?;
    bytes.push(b'\n');
    output.write_all(&bytes).await?;
    output.flush().await
}
fn size(rows: u16, cols: u16) -> io::Result<WindowSize> {
    if rows == 0 || cols == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "terminal size must be nonzero",
        ));
    }
    Ok(WindowSize {
        num_lines: rows,
        num_cols: cols,
        cell_width: 0,
        cell_height: 0,
    })
}

pub async fn run() -> io::Result<i32> {
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    let mut output = tokio::io::stdout();
    let line = input
        .next_line()
        .await?
        .ok_or_else(|| io::Error::other("terminal initialization missing"))?;
    let PtyCommand::Start {
        command,
        cwd,
        rows,
        cols,
    } = serde_json::from_str(&line)?
    else {
        return Err(io::Error::other("expected terminal initialization"));
    };
    let program = command
        .first()
        .ok_or_else(|| io::Error::other("terminal command missing"))?;
    let window_size = size(rows, cols)?;
    let options = Options {
        shell: Some(Shell::new(program.clone(), command[1..].to_vec())),
        working_directory: Some(cwd.into()),
        env: [
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
        ]
        .into(),
        #[cfg(windows)]
        escape_args: true,
        ..Options::default()
    };
    #[allow(unused_mut)]
    let mut pty = tty::new(&options, window_size, 0)?;
    // The caller owns reaping. Dropping the backend on any constructor/loop
    // failure cannot release this session ID before whole-session cleanup.
    #[cfg(unix)]
    let mut session = Some(session::Session::new(
        pty.take_child().expect("new PTY child"),
    ));
    #[cfg(unix)]
    let mut cleanup = None;
    let (send, mut events) = tokio::sync::mpsc::channel(16);
    let listener = Events {
        output: send,
        replies: Arc::default(),
    };
    let screen = Arc::new(FairMutex::new(Term::new(
        Config::default(),
        &window_size,
        listener.clone(),
    )));
    let event_loop = EventLoop::new(screen, listener, pty, true, false)?;
    let commands = event_loop.channel();
    emit(&mut output, PtyEvent::Started).await?;
    let worker = event_loop.spawn();
    let mut exit_code = 0;
    let interaction: io::Result<()> = async {
        loop {
            tokio::select! {
                event = events.recv() => {
                    match event.ok_or_else(|| io::Error::other("terminal event loop disappeared"))? {
                        Event::PtyOutput(data) => emit(&mut output, PtyEvent::Output { data }).await?,
                        Event::Checkpoint { id, data, size } => emit(&mut output, PtyEvent::Checkpoint { id, data, rows: size.num_lines, cols: size.num_cols }).await?,
                        Event::OperationComplete { id, error } => emit(&mut output, PtyEvent::Ack { id, error }).await?,
                        Event::PtyError(message) => return Err(io::Error::other(message)),
                        Event::ChildExit(status) => {
                            exit_code = status.code().unwrap_or(1) as u32;
                            #[cfg(unix)]
                            if let Some(mut owned) = session.take() {
                                cleanup = Some(tokio::task::spawn_blocking(move || owned.finish()));
                            }
                        }
                        Event::Exit => return Ok(()),
                        _ => unreachable!(),
                    }
                }
                line = input.next_line() => {
                    let Some(line) = line? else { return Ok(()); };
                    let command: PtyCommand = serde_json::from_str(&line)?;
                    let (id, command) = match command {
                        PtyCommand::Write { id, data } => (id, Ok(Msg::InputWithAck { id, data: data.into() })),
                        PtyCommand::Resize { id, rows, cols } => (id, size(rows, cols).map(|size| Msg::ResizeWithAck { id, size })),
                        PtyCommand::Checkpoint { id, rows, cols } => (id, size(rows, cols).map(|size| Msg::Checkpoint { id, size })),
                        PtyCommand::Start { .. } => return Err(io::Error::other("terminal already started")),
                    };
                    if let Err(error) = command.and_then(|command| commands.send(command).map_err(io::Error::other)) {
                        emit(&mut output, PtyEvent::Ack { id, error: Some(error.to_string()) }).await?;
                    }
                }
            }
        }
    }.await;
    // Shutdown is out-of-band from the bounded command queue. Stop forwarding
    // before joining so a full output queue cannot hold the loop during cleanup.
    let _ = commands.send(Msg::Shutdown);
    drop(events);
    #[cfg(unix)]
    let cleanup_result = {
        let cleanup = cleanup.unwrap_or_else(|| {
            let mut session = session.take().expect("session cleanup owner");
            tokio::task::spawn_blocking(move || session.finish())
        });
        cleanup.await.map_err(io::Error::other)?
    };
    #[cfg(not(unix))]
    let cleanup_result = Ok::<(), io::Error>(());
    // Closing ConPTY happens after output draining; the Host's existing Job
    // Object remains the Windows supervisor/descendant ownership boundary.
    let joined = tokio::task::spawn_blocking(move || worker.join())
        .await
        .map_err(io::Error::other)?;
    drop(joined.map_err(|_| io::Error::other("terminal event loop panicked"))?);
    cleanup_result?;
    match interaction {
        Ok(()) => {
            let _ = emit(&mut output, PtyEvent::Exited { code: exit_code }).await;
        }
        Err(error) => {
            let _ = emit(
                &mut output,
                PtyEvent::Failed {
                    message: error.to_string(),
                },
            )
            .await;
        }
    }
    Ok(0)
}
