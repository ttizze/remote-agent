//! Code for running a reader/writer on another thread while driving it through `polling`.

use std::io::prelude::*;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::{io, thread};

use piper::{Reader, Writer, pipe};
use polling::os::iocp::{CompletionPacket, PollerIocpExt};
use polling::{Event, PollMode, Poller};

use crate::thread::spawn_named;

struct Registration {
    interest: Mutex<Option<Interest>>,
    end: PipeEnd,
}

#[derive(Copy, Clone)]
enum PipeEnd {
    Reader,
    Writer,
}

struct Interest {
    /// The event to send about completion.
    event: Event,

    /// The poller to send the event to.
    poller: Arc<Poller>,

    /// The mode that we are in.
    mode: PollMode,
}

/// Poll a reader in another thread.
pub struct UnblockedReader<R> {
    /// The event to send about completion.
    interest: Arc<Registration>,

    /// The pipe that we are reading from.
    pipe: Reader,

    /// Is this the first time registering?
    first_register: bool,

    /// Keep draining native output after the event loop stops consuming it.
    discard: Arc<AtomicBool>,
    worker: thread::Thread,

    /// We logically own the reader, but we don't actually use it.
    _reader: PhantomData<R>,
}

impl<R: Read + Send + 'static> UnblockedReader<R> {
    /// Spawn a new unblocked reader.
    pub fn new(mut source: R, pipe_capacity: usize) -> Self {
        // Create a new pipe.
        let (reader, mut writer) = pipe(pipe_capacity);
        let interest = Arc::new(Registration {
            interest: Mutex::<Option<Interest>>::new(None),
            end: PipeEnd::Reader,
        });

        let discard = Arc::new(AtomicBool::new(false));
        let reader_discard = discard.clone();

        // Spawn the reader thread.
        let worker = spawn_named("alacritty-tty-reader-thread", move || {
            let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
            let mut context = Context::from_waker(&waker);

            loop {
                if reader_discard.load(Ordering::Acquire) {
                    // ClosePseudoConsole waits for its native output pipe to drain.
                    // The event loop has stopped, so a full piper queue must not
                    // prevent reading that output through EOF during shutdown.
                    let _ = io::copy(&mut source, &mut io::sink());
                    return;
                }

                // Read from the reader into the pipe.
                match writer.poll_fill(&mut context, &mut source) {
                    Poll::Ready(Ok(0)) => {
                        // Either the pipe is closed or the reader is at its EOF.
                        // In any case, we are done.
                        return;
                    },

                    Poll::Ready(Ok(_)) => {
                        // Keep reading.
                        continue;
                    },

                    Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {
                        // We were interrupted; continue.
                        continue;
                    },

                    Poll::Ready(Err(e)) => {
                        log::error!("error writing to pipe: {}", e);
                        return;
                    },

                    Poll::Pending => {
                        // We are now waiting on the other end to advance. Park the
                        // thread until they do.
                        thread::park();
                    },
                }
            }
        });

        Self {
            interest,
            pipe: reader,
            first_register: true,
            discard,
            worker: worker.thread().clone(),
            _reader: PhantomData,
        }
    }

    /// Stop buffering output and drain the native pipe until it closes.
    pub fn drain_on_shutdown(&self) {
        self.discard.store(true, Ordering::Release);
        self.worker.unpark();
    }

    /// Register interest in the reader.
    pub fn register(&mut self, poller: &Arc<Poller>, event: Event, mode: PollMode) {
        let mut interest = self.interest.interest.lock().unwrap();
        *interest = Some(Interest { event, poller: poller.clone(), mode });

        // Send the event to start off with if we have any data.
        if (!self.pipe.is_empty() && event.readable) || self.first_register {
            self.first_register = false;
            poller.post(CompletionPacket::new(event)).ok();
        }
    }

    /// Deregister interest in the reader.
    pub fn deregister(&self) {
        let mut interest = self.interest.interest.lock().unwrap();
        *interest = None;
    }

}

impl<R: Read + Send + 'static> Read for UnblockedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let waker = Waker::from(self.interest.clone());

        match self.pipe.poll_drain_bytes(&mut Context::from_waker(&waker), buf) {
            // An in-flight native read is not EOF. In particular, the child can
            // exit before its final output reaches this buffer.
            Poll::Pending => Err(io::ErrorKind::WouldBlock.into()),
            Poll::Ready(n) => Ok(n),
        }
    }
}

/// Status shared with the blocking writer thread.
#[derive(Default)]
struct WriteState {
    /// A failed native write/flush is terminal. Keep it for subsequent calls too.
    error: Option<Arc<io::Error>>,

    /// Wait for native completion instead of reporting spare queue capacity.
    flushing: bool,
}

impl WriteState {
    fn check_error(&self) -> io::Result<()> {
        match &self.error {
            Some(error) => Err(match error.raw_os_error() {
                Some(code) => io::Error::from_raw_os_error(code),
                None => io::Error::new(error.kind(), error.clone()),
            }),
            None => Ok(()),
        }
    }
}

/// Poll a writer in another thread.
pub struct UnblockedWriter<W> {
    /// The interest to send about completion.
    interest: Arc<Registration>,

    /// The pipe that we are writing to. Bytes remain here until the native write
    /// and flush finish, so outstanding writes are bounded by its capacity.
    pipe: Writer,

    state: Arc<Mutex<WriteState>>,

    /// We logically own the writer, but we don't actually use it.
    _writer: PhantomData<W>,
}

impl<W: Write + Send + 'static> UnblockedWriter<W> {
    /// Spawn a new unblocked writer.
    pub fn new(mut sink: W, pipe_capacity: usize) -> Self {
        let (mut reader, writer) = pipe(pipe_capacity);
        let interest = Arc::new(Registration {
            interest: Mutex::<Option<Interest>>::new(None),
            end: PipeEnd::Writer,
        });
        let state = Arc::new(Mutex::new(WriteState::default()));
        let writer_interest = interest.clone();
        let writer_state = state.clone();

        spawn_named("alacritty-tty-writer-thread", move || {
            let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
            let mut context = Context::from_waker(&waker);

            loop {
                match reader.poll(&mut context) {
                    Poll::Ready(false) => return,
                    Poll::Pending => {
                        thread::park();
                        continue;
                    }
                    Poll::Ready(true) => (),
                }

                // Do not hold a lock across a potentially blocking native call.
                let result = match sink.write(reader.peek_buf()) {
                    Ok(0) => Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "PTY write returned zero",
                    )),
                    Ok(n) => {
                        // A successful flush is part of write completion. Retry only
                        // the flush if interrupted, since these bytes were written.
                        let flushed = loop {
                            match sink.flush() {
                                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                                result => break result,
                            }
                        };
                        flushed.map(|()| n)
                    }
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                    Err(err) => Err(err),
                };

                match result {
                    Ok(n) => reader.consume(n),
                    Err(err) => {
                        writer_state.lock().unwrap().error = Some(Arc::new(err));
                        // The error must be visible before the completion wakeup.
                        writer_interest.wake_by_ref();
                        return;
                    }
                }

                // Piper only wakes a producer that previously ran out of space.
                // Flush may be waiting even when the pipe has never been full.
                writer_interest.wake_by_ref();
            }
        });

        Self {
            interest,
            pipe: writer,
            state,
            _writer: PhantomData,
        }
    }

    /// Register interest in the writer.
    pub fn register(&self, poller: &Arc<Poller>, event: Event, mode: PollMode) {
        // Always acquire state before registration. A piper operation made with
        // state locked can synchronously wake Registration too.
        let state = self.state.lock().unwrap();
        let mut interest = self.interest.interest.lock().unwrap();
        *interest = Some(Interest {
            event,
            poller: poller.clone(),
            mode,
        });

        let ready = state.error.is_some()
            || if state.flushing {
                self.pipe.is_empty()
            } else {
                !self.pipe.is_full()
            };
        if ready && event.writable {
            poller.post(CompletionPacket::new(event)).ok();
            if matches!(mode, PollMode::Oneshot | PollMode::EdgeOneshot) {
                *interest = None;
            }
        }
    }

    /// Deregister interest in the writer.
    pub fn deregister(&self) {
        let mut interest = self.interest.interest.lock().unwrap();
        *interest = None;
    }
}

impl<W: Write + Send + 'static> Write for UnblockedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut state = self.state.lock().unwrap();
        state.check_error()?;
        if buf.is_empty() {
            return Ok(0);
        }
        state.flushing = false;
        let waker = Waker::from(self.interest.clone());

        match self.pipe.poll(&mut Context::from_waker(&waker)) {
            Poll::Pending => Err(io::ErrorKind::WouldBlock.into()),
            Poll::Ready(false) => Err(io::ErrorKind::BrokenPipe.into()),
            Poll::Ready(true) => {
                let target = self.pipe.write_buf(buf.len());
                let n = target.len();
                target.copy_from_slice(&buf[..n]);
                self.pipe.produced(n);
                Ok(n)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        state.check_error()?;
        state.flushing = !self.pipe.is_empty();
        if state.flushing {
            Err(io::ErrorKind::WouldBlock.into())
        } else {
            Ok(())
        }
    }
}

struct ThreadWaker(thread::Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

impl Wake for Registration {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let mut interest_lock = self.interest.lock().unwrap();
        if let Some(interest) = interest_lock.as_ref() {
            // Send the event to the poller.
            let send_event = match self.end {
                PipeEnd::Reader => interest.event.readable,
                PipeEnd::Writer => interest.event.writable,
            };

            if send_event {
                interest.poller.post(CompletionPacket::new(interest.event)).ok();

                // Clear the event if we're in oneshot mode.
                if matches!(interest.mode, PollMode::Oneshot | PollMode::EdgeOneshot) {
                    *interest_lock = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::Duration;

    use polling::Events;

    const TIMEOUT: Duration = Duration::from_secs(5);
    const TOKEN: usize = 42;

    struct ControlledWriter {
        calls: Sender<Vec<u8>>,
        writes: Receiver<io::Result<usize>>,
        flushes: Sender<()>,
        flush_results: Receiver<io::Result<()>>,
    }

    struct Controller {
        calls: Receiver<Vec<u8>>,
        writes: Sender<io::Result<usize>>,
        flushes: Receiver<()>,
        flush_results: Sender<io::Result<()>>,
    }

    impl Write for ControlledWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.calls.send(buf.to_vec()).unwrap();
            self.writes.recv_timeout(TIMEOUT).unwrap()
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes.send(()).unwrap();
            self.flush_results.recv_timeout(TIMEOUT).unwrap()
        }
    }

    fn controlled_writer(capacity: usize) -> (UnblockedWriter<ControlledWriter>, Controller) {
        let (calls_tx, calls_rx) = mpsc::channel();
        let (writes_tx, writes_rx) = mpsc::channel();
        let (flushes_tx, flushes_rx) = mpsc::channel();
        let (flush_results_tx, flush_results_rx) = mpsc::channel();
        let sink = ControlledWriter {
            calls: calls_tx,
            writes: writes_rx,
            flushes: flushes_tx,
            flush_results: flush_results_rx,
        };
        let controller = Controller {
            calls: calls_rx,
            writes: writes_tx,
            flushes: flushes_rx,
            flush_results: flush_results_tx,
        };
        (UnblockedWriter::new(sink, capacity), controller)
    }

    fn wait_for_completion(poller: &Poller) {
        let mut events = Events::new();
        poller.wait(&mut events, Some(TIMEOUT)).unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.key == TOKEN && event.writable)
        );
    }

    struct FiniteReader {
        bytes: io::Cursor<Vec<u8>>,
        consumed: Sender<usize>,
        gate: Option<(Sender<()>, Receiver<()>)>,
    }

    impl Read for FiniteReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if buf.is_empty() {
                return Ok(0);
            }
            if let Some((entered, release)) = self.gate.take() {
                entered.send(()).unwrap();
                release.recv_timeout(TIMEOUT).unwrap();
            }
            self.bytes.read(buf)
        }
    }

    impl Drop for FiniteReader {
        fn drop(&mut self) {
            self.consumed.send(self.bytes.position() as usize).ok();
        }
    }

    #[test]
    fn delayed_native_output_is_pending_until_data_then_true_eof() {
        let (consumed_tx, _consumed_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let source = FiniteReader {
            bytes: io::Cursor::new(b"late native output".to_vec()),
            consumed: consumed_tx,
            gate: Some((entered_tx, release_rx)),
        };
        let mut reader = UnblockedReader::new(source, 4);
        entered_rx.recv_timeout(TIMEOUT).unwrap();
        let poller = Arc::new(Poller::new().unwrap());
        reader.register(&poller, Event::readable(TOKEN), PollMode::Level);
        let mut buffer = [0; 8];
        assert_eq!(reader.read(&mut buffer).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        release_tx.send(()).unwrap();

        let deadline = std::time::Instant::now() + TIMEOUT;
        let mut output = Vec::new();
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => output.extend_from_slice(&buffer[..n]),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    let remaining = deadline.checked_duration_since(std::time::Instant::now())
                        .expect("reader did not reach EOF");
                    let mut events = Events::new();
                    poller.wait(&mut events, Some(remaining)).unwrap();
                    assert!(events.iter().any(|event| event.key == TOKEN && event.readable));
                },
                Err(err) => panic!("unexpected read error: {err}"),
            }
        }
        assert_eq!(output, b"late native output");
        assert_eq!(reader.read(&mut buffer).unwrap(), 0);
    }

    #[test]
    fn shutdown_drains_native_output_when_buffer_is_full() {
        let (consumed_tx, consumed_rx) = mpsc::channel();
        let source = FiniteReader {
            bytes: io::Cursor::new(vec![0; 32]),
            consumed: consumed_tx,
            gate: None,
        };
        let reader = UnblockedReader::new(source, 4);
        let deadline = std::time::Instant::now() + TIMEOUT;
        while !reader.pipe.is_full() {
            assert!(std::time::Instant::now() < deadline, "reader did not fill the pipe");
            thread::yield_now();
        }
        reader.drain_on_shutdown();
        assert_eq!(consumed_rx.recv_timeout(TIMEOUT).unwrap(), 32);
    }

    #[test]
    fn shutdown_drains_after_an_in_flight_native_read() {
        let (consumed_tx, consumed_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let source = FiniteReader {
            bytes: io::Cursor::new(vec![0; 32]),
            consumed: consumed_tx,
            gate: Some((entered_tx, release_rx)),
        };
        let reader = UnblockedReader::new(source, 4);
        entered_rx.recv_timeout(TIMEOUT).unwrap();
        reader.drain_on_shutdown();
        release_tx.send(()).unwrap();
        assert_eq!(consumed_rx.recv_timeout(TIMEOUT).unwrap(), 32);
    }

    #[test]
    fn registration_reports_available_capacity() {
        let (writer, _control) = controlled_writer(8);
        let poller = Arc::new(Poller::new().unwrap());
        writer.register(&poller, Event::writable(TOKEN), PollMode::Oneshot);
        wait_for_completion(&poller);
    }

    #[test]
    fn deregistration_suppresses_native_completion_notifications() {
        let (mut writer, control) = controlled_writer(8);
        assert_eq!(writer.write(b"data").unwrap(), 4);
        control.calls.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(writer.flush().unwrap_err().kind(), io::ErrorKind::WouldBlock);
        let poller = Arc::new(Poller::new().unwrap());
        writer.register(&poller, Event::writable(TOKEN), PollMode::Oneshot);
        writer.deregister();

        // Closing the producer still lets the pending native operation drain.
        // The sink's sender disconnects once the writer thread has finished,
        // making the absence of a completion notification observable.
        drop(writer);
        control.writes.send(Ok(4)).unwrap();
        control.flushes.recv_timeout(TIMEOUT).unwrap();
        control.flush_results.send(Ok(())).unwrap();
        assert_eq!(
            control.calls.recv_timeout(TIMEOUT).unwrap_err(),
            mpsc::RecvTimeoutError::Disconnected,
        );
        let mut events = Events::new();
        poller.wait(&mut events, Some(Duration::from_millis(20))).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn flush_waits_for_native_write_and_flush_without_busy_polling() {
        let (mut writer, control) = controlled_writer(16);
        let poller = Arc::new(Poller::new().unwrap());
        assert_eq!(writer.write(b"hello").unwrap(), 5);
        assert_eq!(control.calls.recv_timeout(TIMEOUT).unwrap(), b"hello");
        assert_eq!(
            writer.flush().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        writer.register(&poller, Event::writable(TOKEN), PollMode::Level);

        // The ring has room, but a pending flush must await native completion.
        let mut events = Events::new();
        poller
            .wait(&mut events, Some(Duration::from_millis(20)))
            .unwrap();
        assert!(events.is_empty());

        control.writes.send(Ok(5)).unwrap();
        control.flushes.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(
            writer.flush().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        control.flush_results.send(Ok(())).unwrap();
        wait_for_completion(&poller);
        writer.flush().unwrap();
    }

    #[test]
    fn partial_native_writes_retain_order_and_bounded_capacity() {
        let (mut writer, control) = controlled_writer(4);
        assert_eq!(writer.write(b"abcdef").unwrap(), 4);
        assert_eq!(control.calls.recv_timeout(TIMEOUT).unwrap(), b"abcd");
        assert_eq!(
            writer.write(b"ef").unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let poller = Arc::new(Poller::new().unwrap());
        writer.register(&poller, Event::writable(TOKEN), PollMode::Oneshot);

        control.writes.send(Ok(2)).unwrap();
        control.flushes.recv_timeout(TIMEOUT).unwrap();
        control.flush_results.send(Ok(())).unwrap();
        wait_for_completion(&poller);
        assert_eq!(control.calls.recv_timeout(TIMEOUT).unwrap(), b"cd");
        assert_eq!(writer.write(b"ef").unwrap(), 2);
        assert_eq!(
            writer.flush().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );

        control.writes.send(Ok(2)).unwrap();
        control.flushes.recv_timeout(TIMEOUT).unwrap();
        control.flush_results.send(Ok(())).unwrap();
        assert_eq!(control.calls.recv_timeout(TIMEOUT).unwrap(), b"ef");
        assert_eq!(
            writer.flush().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        writer.register(&poller, Event::writable(TOKEN), PollMode::Oneshot);
        control.writes.send(Ok(2)).unwrap();
        control.flushes.recv_timeout(TIMEOUT).unwrap();
        control.flush_results.send(Ok(())).unwrap();
        wait_for_completion(&poller);
        writer.flush().unwrap();
    }

    #[test]
    fn native_errors_survive_write_flush_and_late_registration() {
        let (mut writer, control) = controlled_writer(8);
        assert_eq!(writer.write(b"data").unwrap(), 4);
        control.calls.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(
            writer.flush().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let poller = Arc::new(Poller::new().unwrap());
        writer.register(&poller, Event::writable(TOKEN), PollMode::Oneshot);
        control
            .writes
            .send(Err(io::Error::from_raw_os_error(109)))
            .unwrap();
        wait_for_completion(&poller);
        assert_eq!(writer.flush().unwrap_err().raw_os_error(), Some(109));
        assert_eq!(writer.write(b"more").unwrap_err().raw_os_error(), Some(109));

        // Completion happened before re-registration; the terminal error must
        // still produce readiness even though the ring has not been consumed.
        writer.register(&poller, Event::writable(TOKEN), PollMode::Oneshot);
        wait_for_completion(&poller);
        assert_eq!(writer.flush().unwrap_err().raw_os_error(), Some(109));
    }

    #[test]
    fn zero_native_write_is_reported_instead_of_false_completion() {
        let (mut writer, control) = controlled_writer(8);
        assert_eq!(writer.write(b"data").unwrap(), 4);
        control.calls.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(
            writer.flush().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let poller = Arc::new(Poller::new().unwrap());
        writer.register(&poller, Event::writable(TOKEN), PollMode::Oneshot);
        control.writes.send(Ok(0)).unwrap();
        wait_for_completion(&poller);
        assert_eq!(writer.flush().unwrap_err().kind(), io::ErrorKind::WriteZero);
        assert_eq!(
            writer.write(b"more").unwrap_err().kind(),
            io::ErrorKind::WriteZero
        );
    }

    #[test]
    fn native_flush_error_is_reported() {
        let (mut writer, control) = controlled_writer(8);
        assert_eq!(writer.write(b"data").unwrap(), 4);
        control.calls.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(
            writer.flush().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let poller = Arc::new(Poller::new().unwrap());
        writer.register(&poller, Event::writable(TOKEN), PollMode::Oneshot);
        control.writes.send(Ok(4)).unwrap();
        control.flushes.recv_timeout(TIMEOUT).unwrap();
        control
            .flush_results
            .send(Err(io::Error::other("native flush failed")))
            .unwrap();
        wait_for_completion(&poller);
        assert_eq!(
            writer.flush().unwrap_err().to_string(),
            "native flush failed"
        );
    }

    #[test]
    fn interrupted_native_write_and_flush_are_retried_without_duplication() {
        let (mut writer, control) = controlled_writer(8);
        assert_eq!(writer.write(b"data").unwrap(), 4);
        assert_eq!(control.calls.recv_timeout(TIMEOUT).unwrap(), b"data");
        control
            .writes
            .send(Err(io::ErrorKind::Interrupted.into()))
            .unwrap();
        assert_eq!(control.calls.recv_timeout(TIMEOUT).unwrap(), b"data");
        control.writes.send(Ok(4)).unwrap();
        control.flushes.recv_timeout(TIMEOUT).unwrap();
        control
            .flush_results
            .send(Err(io::ErrorKind::Interrupted.into()))
            .unwrap();
        control.flushes.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(
            writer.flush().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let poller = Arc::new(Poller::new().unwrap());
        writer.register(&poller, Event::writable(TOKEN), PollMode::Oneshot);
        control.flush_results.send(Ok(())).unwrap();
        wait_for_completion(&poller);
        writer.flush().unwrap();
        assert!(control.calls.try_recv().is_err());
    }
}
