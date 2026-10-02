//! The main event loop which performs I/O on the pseudoterminal.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt::{self, Display, Formatter};
use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use polling::{Event as PollingEvent, Events, PollMode, Poller};

use crate::event::{self, Event, EventListener, WindowSize};
use crate::sync::FairMutex;
use crate::term::Term;
use crate::grid::Dimensions;
use crate::{thread, tty};
use vte::ansi;

/// Max bytes to read from the PTY before forced terminal synchronization.
pub(crate) const READ_BUFFER_SIZE: usize = 0x10_0000;

/// Max bytes to read from the PTY while the terminal is locked.
const MAX_LOCKED_READ: usize = u16::MAX as usize;
const COMMAND_CAPACITY: usize = 32;
const WRITE_CAPACITY: usize = 32;
const WRITE_BYTE_CAPACITY: usize = 16 * 1024 * 1024;

/// Messages that may be sent to the `EventLoop`.
#[derive(Debug)]
pub enum Msg {
    /// Data that should be written to the PTY.
    Input(Cow<'static, [u8]>),
    /// Acknowledge only after the native writer has drained this input.
    InputWithAck { id: u64, data: Cow<'static, [u8]> },
    /// Resize the native PTY and terminal before acknowledging.
    ResizeWithAck { id: u64, size: WindowSize },
    /// Resize and capture the screen and partial parser in this I/O loop.
    Checkpoint { id: u64, size: WindowSize },

    /// Indicates that the `EventLoop` should shut down, as Alacritty is shutting down.
    Shutdown,

    /// Instruction to resize the PTY.
    Resize(WindowSize),
}

/// The main event loop.
///
/// Handles all the PTY I/O and runs the PTY parser which updates terminal
/// state.
pub struct EventLoop<T: tty::EventedPty, U: EventListener> {
    poll: Arc<Poller>,
    pty: T,
    rx: Receiver<Msg>,
    tx: SyncSender<Msg>,
    shutdown: Arc<AtomicBool>,
    terminal: Arc<FairMutex<Term<U>>>,
    event_proxy: U,
    drain_on_exit: bool,
    ref_test: bool,
}

impl<T, U> EventLoop<T, U>
where
    T: tty::EventedPty + event::OnResize + Send + 'static,
    U: EventListener + Send + 'static,
{
    /// Create a new event loop.
    pub fn new(
        terminal: Arc<FairMutex<Term<U>>>,
        event_proxy: U,
        pty: T,
        drain_on_exit: bool,
        ref_test: bool,
    ) -> io::Result<EventLoop<T, U>> {
        let (tx, rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let poll = Poller::new()?.into();
        Ok(EventLoop {
            poll,
            pty,
            tx,
            shutdown: Arc::default(),
            rx,
            terminal,
            event_proxy,
            drain_on_exit,
            ref_test,
        })
    }

    pub fn channel(&self) -> EventLoopSender {
        EventLoopSender { sender: self.tx.clone(), poller: self.poll.clone(), shutdown: self.shutdown.clone() }
    }

    /// Drain the channel.
    ///
    /// Returns `false` when a shutdown message was received.
    fn complete(&self, id: u64, result: io::Result<()>) {
        self.event_proxy.send_event(Event::OperationComplete {
            id, error: result.err().map(|error| error.to_string()),
        });
    }

    fn resize(&mut self, size: WindowSize) -> io::Result<()> {
        if size.num_lines == 0 || size.num_cols == 0 {
            return Err(io::Error::new(ErrorKind::InvalidInput, "terminal size must be nonzero"));
        }
        self.pty.on_resize(size)?;
        self.terminal.lock().resize(size);
        Ok(())
    }

    fn drain_recv_channel(&mut self, state: &mut State) -> io::Result<bool> {
        while let Ok(msg) = self.rx.try_recv() {
            if self.shutdown.load(Ordering::Acquire) { return Ok(false); }
            if state.exited {
                match msg {
                    Msg::InputWithAck { id, .. } | Msg::ResizeWithAck { id, .. } | Msg::Checkpoint { id, .. } => {
                        self.complete(id, Err(io::Error::new(ErrorKind::BrokenPipe, "terminal exited")));
                    },
                    Msg::Shutdown => return Ok(false),
                    _ => {},
                }
                continue;
            }
            match msg {
                Msg::Input(input) => state.queue(Writing::new(input, None))?,
                Msg::InputWithAck { id, data } => {
                    if let Err(error) = state.queue(Writing::new(data, Some(id))) {
                        self.complete(id, Err(error));
                    }
                },
                Msg::Resize(size) => self.resize(size)?,
                Msg::ResizeWithAck { id, size } => {
                    let result = self.resize(size);
                    self.complete(id, result);
                },
                Msg::Checkpoint { id, size } => {
                    if let Err(error) = self.resize(size) {
                        self.complete(id, Err(error));
                        continue;
                    }
                    let terminal = self.terminal.lock();
                    let mut data = terminal.ansi_checkpoint(state.parser.preceding_char());
                    data.extend(state.parser.checkpoint_tail());
                    self.event_proxy.send_event(Event::Checkpoint { id, data, size });
                },
                Msg::Shutdown => return Ok(false),
            }
        }
        Ok(true)
    }

    #[inline]
    fn pty_read<X>(
        &mut self,
        state: &mut State,
        buf: &mut [u8],
        mut writer: Option<&mut X>,
    ) -> io::Result<usize>
    where
        X: Write,
    {
        let mut unprocessed = 0;
        let mut processed = 0;

        // Reserve the next terminal lock for PTY reading.
        let _terminal_lease = Some(self.terminal.lease());
        let mut terminal = None;

        loop {
            // Read from the PTY.
            match self.pty.reader().read(&mut buf[unprocessed..]) {
                // This is received on Windows/macOS when no more data is readable from the PTY.
                Ok(0) => {
                    state.read_closed = true;
                    if unprocessed == 0 { break; }
                },
                Ok(got) => unprocessed += got,
                Err(err) => match err.kind() {
                    ErrorKind::Interrupted | ErrorKind::WouldBlock => {
                        // Go back to mio if we're caught up on parsing and the PTY would block.
                        if unprocessed == 0 {
                            break;
                        }
                    },
                    #[cfg(target_os = "linux")]
                    _ if err.raw_os_error() == Some(libc::EIO) => {
                        state.read_closed = true;
                        if unprocessed == 0 { break; }
                    },
                    _ => return Err(err),
                },
            }

            // Attempt to lock the terminal.
            let terminal = match &mut terminal {
                Some(terminal) => terminal,
                None => terminal.insert(match self.terminal.try_lock_unfair() {
                    // Force block if we are at the buffer size limit.
                    None if unprocessed >= READ_BUFFER_SIZE => self.terminal.lock_unfair(),
                    None => continue,
                    Some(terminal) => terminal,
                }),
            };

            // Write a copy of the bytes to the ref test file.
            if let Some(writer) = &mut writer {
                writer.write_all(&buf[..unprocessed]).unwrap();
            }

            // Parse the incoming bytes.
            state.parser.advance(&mut **terminal, &buf[..unprocessed]);
            let reply = self.event_proxy.take_replies(terminal);
            if !reply.is_empty() {
                state.queue(Writing::new(reply.into(), None))?;
            }
            self.event_proxy.send_event(Event::PtyOutput(buf[..unprocessed].to_vec()));

            processed += unprocessed;
            unprocessed = 0;

            // Assure we're not blocking the terminal too long unnecessarily.
            if processed >= MAX_LOCKED_READ {
                break;
            }
        }

        // Queue terminal redraw unless all processed bytes were synchronized.
        if state.parser.sync_bytes_count() < processed && processed > 0 {
            self.event_proxy.send_event(Event::Wakeup);
        }

        Ok(processed)
    }

    #[inline]
    fn pty_write(&mut self, state: &mut State) -> io::Result<()> {
        state.ensure_next();
        while let Some(mut current) = state.take_current() {
            let result = (|| {
                while !current.finished() {
                    match self.pty.writer().write(current.remaining_bytes()) {
                        Ok(0) => return Err(ErrorKind::WouldBlock.into()),
                        Ok(n) => current.advance(n),
                        Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                        Err(error) => return Err(error),
                    }
                }
                // Windows buffers writes on a worker thread. Its nonblocking
                // flush reports actual native completion/error, not enqueueing.
                self.pty.writer().flush()
            })();
            match result {
                Ok(()) => {
                    if let Some(id) = current.id { self.complete(id, Ok(())); }
                    state.goto_next();
                },
                Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
                    state.set_current(Some(current));
                    return Ok(());
                },
                Err(error) => {
                    if let Some(id) = current.id {
                        // Like the supervisor's previous write worker, report the
                        // operation failure without truncating independent output.
                        self.complete(id, Err(error));
                        state.goto_next();
                    } else {
                        return Err(error);
                    }
                },
            }
        }
        Ok(())
    }

    pub fn spawn(mut self) -> JoinHandle<(Self, State)> {
        thread::spawn_named("PTY reader", move || {
            let mut state = State::default();
            let mut buf = [0u8; READ_BUFFER_SIZE];

            let poll_opts = PollMode::Level;
            let mut interest = PollingEvent::readable(0);

            // Register TTY through EventedRW interface.
            if let Err(err) = unsafe { self.pty.register(&self.poll, interest, poll_opts) } {
                self.event_proxy.send_event(Event::PtyError(err.to_string()));
                self.event_proxy.send_event(Event::Exit);
                return (self, state);
            }

            let mut events = Events::with_capacity(NonZeroUsize::new(1024).unwrap());

            let mut pipe = if self.ref_test {
                Some(File::create("./alacritty.recording").expect("create alacritty recording"))
            } else {
                None
            };

            let mut drain_deadline: Option<Instant> = None;
            'event_loop: loop {
                if self.shutdown.load(Ordering::Acquire) { break; }
                // Wakeup the event loop when a synchronized update timeout was reached.
                if drain_deadline.is_some_and(|deadline| state.read_closed || Instant::now() >= deadline) {
                    break;
                }
                let deadline = match (state.parser.sync_timeout().sync_timeout(), drain_deadline) {
                    (Some(sync), Some(drain)) => Some(sync.min(drain)),
                    (sync, drain) => sync.or(drain),
                };
                let timeout = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));

                events.clear();
                if let Err(err) = self.poll.wait(&mut events, timeout) {
                    match err.kind() {
                        ErrorKind::Interrupted => continue,
                        _ => {
                            self.event_proxy.send_event(Event::PtyError(err.to_string()));
                            break 'event_loop;
                        },
                    }
                }

                // Handle synchronized update timeout.
                if state.parser.sync_timeout().sync_timeout().is_some_and(|deadline| Instant::now() >= deadline) {
                    let terminal = &mut *self.terminal.lock();
                    state.parser.stop_sync(terminal);
                    let reply = self.event_proxy.take_replies(terminal);
                    let error = if reply.is_empty() { None } else {
                        state.queue(Writing::new(reply.into(), None)).err()
                    };
                    self.event_proxy.send_event(Event::Wakeup);
                    if let Some(error) = error {
                        self.event_proxy.send_event(Event::PtyError(error.to_string()));
                        break;
                    }
                    // Pending timeout-generated query replies need writable interest below.
                }

                // Handle channel events, if there are any.
                match self.drain_recv_channel(&mut state) {
                    Ok(true) => {},
                    Ok(false) => break,
                    Err(error) => {
                        self.event_proxy.send_event(Event::PtyError(error.to_string()));
                        break;
                    },
                }

                for event in events.iter() {
                    match event.key {
                        tty::PTY_CHILD_EVENT_TOKEN => {
                            if let Some(tty::ChildEvent::Exited(status)) =
                                self.pty.next_child_event()
                            {
                                state.exited = true;
                                // A failed write after child exit must not abort the
                                // independent final-output drain.
                                state.fail_writes(&self.event_proxy);
                                if let Some(status) = status {
                                    self.event_proxy.send_event(Event::ChildExit(status));
                                } else {
                                    self.event_proxy.send_event(Event::PtyError("terminal exited without a status".into()));
                                    break 'event_loop;
                                }
                                if !self.drain_on_exit { break 'event_loop; }
                                // A Windows reader may still be filling its bounded buffer
                                // after the child-exit callback. Wait for true EOF, retaining
                                // Bex's existing one-second final-output bound.
                                drain_deadline = Some(Instant::now() + Duration::from_secs(1));
                                let _ = self.pty_read(&mut state, &mut buf, pipe.as_mut());
                                self.event_proxy.send_event(Event::Wakeup);
                            }
                        },

                        tty::PTY_READ_WRITE_TOKEN => {
                            if event.is_interrupt() {
                                // Don't try to do I/O on a dead PTY.
                                continue;
                            }

                            if event.readable {
                                if let Err(err) = self.pty_read(&mut state, &mut buf, pipe.as_mut())
                                {
                                    // On Linux, a `read` on the master side of a PTY can fail
                                    // with `EIO` if the client side hangs up.  In that case,
                                    // just loop back round for the inevitable `Exited` event.
                                    // This sucks, but checking the process is either racy or
                                    // blocking.
                                    #[cfg(target_os = "linux")]
                                    if err.raw_os_error() == Some(libc::EIO) {
                                        continue;
                                    }

                                    self.event_proxy.send_event(Event::PtyError(err.to_string()));
                                    break 'event_loop;
                                }
                            }

                            if event.writable {
                                if let Err(err) = self.pty_write(&mut state) {
                                    self.event_proxy.send_event(Event::PtyError(err.to_string()));
                                    break 'event_loop;
                                }
                            }
                        },
                        _ => (),
                    }
                }

                // Register write interest if necessary.
                let needs_write = state.needs_write();
                let needs_read = !state.read_closed;
                if needs_write != interest.writable || needs_read != interest.readable {
                    interest.writable = needs_write;
                    interest.readable = needs_read;

                    // Re-register with new interest.
                    if let Err(error) = self.pty.reregister(&self.poll, interest, poll_opts) {
                        self.event_proxy.send_event(Event::PtyError(error.to_string()));
                        break;
                    }
                }
            }

            state.fail_writes(&self.event_proxy);
            self.event_proxy.send_event(Event::Exit);
            // The evented instances are not dropped here so deregister them explicitly.
            let _ = self.pty.deregister(&self.poll);

            (self, state)
        })
    }
}

/// Helper type which tracks how much of a buffer has been written.
struct Writing {
    id: Option<u64>,
    source: Cow<'static, [u8]>,
    written: usize,
}

pub struct Notifier(pub EventLoopSender);

impl event::Notify for Notifier {
    fn notify<B>(&self, bytes: B)
    where
        B: Into<Cow<'static, [u8]>>,
    {
        let bytes = bytes.into();
        // Terminal hangs if we send 0 bytes through.
        if bytes.is_empty() {
            return;
        }

        let _ = self.0.send(Msg::Input(bytes));
    }
}

impl event::OnResize for Notifier {
    fn on_resize(&mut self, window_size: WindowSize) -> io::Result<()> {
        self.0.send(Msg::Resize(window_size)).map_err(io::Error::other)
    }
}

#[derive(Debug)]
pub enum EventLoopSendError {
    /// Error polling the event loop.
    Io(io::Error),

    /// Error sending a message to the event loop.
    Send(mpsc::TrySendError<Msg>),
}

impl Display for EventLoopSendError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            EventLoopSendError::Io(err) => err.fmt(f),
            EventLoopSendError::Send(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for EventLoopSendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EventLoopSendError::Io(err) => err.source(),
            EventLoopSendError::Send(err) => err.source(),
        }
    }
}

#[derive(Clone)]
pub struct EventLoopSender {
    sender: SyncSender<Msg>,
    shutdown: Arc<AtomicBool>,
    poller: Arc<Poller>,
}

impl EventLoopSender {
    pub fn send(&self, msg: Msg) -> Result<(), EventLoopSendError> {
        if matches!(msg, Msg::Shutdown) {
            // Shutdown must remain deliverable even when the bounded queue is full.
            self.shutdown.store(true, Ordering::Release);
        } else {
            self.sender.try_send(msg).map_err(EventLoopSendError::Send)?;
        }
        self.poller.notify().map_err(EventLoopSendError::Io)
    }
}

/// All of the mutable state needed to run the event loop.
///
/// Contains list of items to write, current write state, etc. Anything that
/// would otherwise be mutated on the `EventLoop` goes here.
#[derive(Default)]
pub struct State {
    write_list: VecDeque<Writing>,
    writing: Option<Writing>,
    parser: ansi::Processor,
    read_closed: bool,
    exited: bool,
}

impl State {
    fn fail_writes(&mut self, listener: &impl EventListener) {
        for writing in self.writing.take().into_iter().chain(self.write_list.drain(..)) {
            if let Some(id) = writing.id {
                listener.send_event(Event::OperationComplete {
                    id, error: Some("terminal closed before write completed".into()),
                });
            }
        }
    }

    fn queue(&mut self, writing: Writing) -> io::Result<()> {
        if self.exited { return Ok(()); }
        let bytes: usize = self.writing.iter().chain(&self.write_list)
            .map(|writing| writing.source.len()).sum();
        if writing.source.len() > WRITE_BYTE_CAPACITY.saturating_sub(bytes) {
            return Err(io::Error::new(ErrorKind::WouldBlock, "terminal write byte budget exceeded"));
        }
        // Palette/query bursts are one ordered byte stream, not one queue slot
        // per query. Coalesce adjacent unacknowledged replies across reads too.
        if writing.id.is_none()
            && let Some(last) = self.write_list.back_mut().filter(|last| last.id.is_none())
        {
            last.source.to_mut().extend_from_slice(&writing.source);
            return Ok(());
        }
        if self.write_list.len() + usize::from(self.writing.is_some()) >= WRITE_CAPACITY {
            return Err(io::Error::new(ErrorKind::WouldBlock, "terminal write queue is full"));
        }
        self.write_list.push_back(writing);
        Ok(())
    }

    #[inline]
    fn ensure_next(&mut self) {
        if self.writing.is_none() {
            self.goto_next();
        }
    }

    #[inline]
    fn goto_next(&mut self) {
        self.writing = self.write_list.pop_front();
    }

    #[inline]
    fn take_current(&mut self) -> Option<Writing> {
        self.writing.take()
    }

    #[inline]
    fn needs_write(&self) -> bool {
        self.writing.is_some() || !self.write_list.is_empty()
    }

    #[inline]
    fn set_current(&mut self, new: Option<Writing>) {
        self.writing = new;
    }
}

impl Writing {
    #[inline]
    fn new(c: Cow<'static, [u8]>, id: Option<u64>) -> Writing {
        Writing { source: c, written: 0, id }
    }

    #[inline]
    fn advance(&mut self, n: usize) {
        self.written += n;
    }

    #[inline]
    fn remaining_bytes(&self) -> &[u8] {
        &self.source[self.written..]
    }

    #[inline]
    fn finished(&self) -> bool {
        self.written >= self.source.len()
    }
}

impl Dimensions for WindowSize {
    fn total_lines(&self) -> usize { self.num_lines as usize }
    fn screen_lines(&self) -> usize { self.num_lines as usize }
    fn columns(&self) -> usize { self.num_cols as usize }
}
