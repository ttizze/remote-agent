//! Numeric socket boundaries and kernel counters for Bex connection diagnostics.
use std::{
    io,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Wake, Waker},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
};

#[derive(Debug)]
struct IoWake {
    span: tracing::Span,
    phase: &'static str,
    upstream: Mutex<Waker>,
}
impl Wake for IoWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.span
            .in_scope(|| tracing::trace!(target: "bex.net.packet", phase = self.phase));
        // Release the lock before forwarding into an arbitrary executor.
        let waker = self.upstream.lock().unwrap().clone();
        waker.wake();
    }
}
impl IoWake {
    fn new(span: &tracing::Span, phase: &'static str) -> Arc<Self> {
        Arc::new(Self {
            span: span.clone(),
            phase,
            upstream: Mutex::new(Waker::noop().clone()),
        })
    }
    fn waker(self: &Arc<Self>, upstream: &Waker) -> Waker {
        self.upstream.lock().unwrap().clone_from(upstream);
        Waker::from(self.clone())
    }
}

#[derive(Debug)]
pub(crate) struct ObservedTcp {
    socket: TcpStream,
    capture: Option<Capture>,
}

#[derive(Debug)]
struct Capture {
    span: tracing::Span,
    read: Arc<IoWake>,
    write: Arc<IoWake>,
    read_bytes: u64,
    written_bytes: u64,
    #[cfg(target_vendor = "apple")]
    probe: Arc<Mutex<Option<std::os::fd::RawFd>>>,
}

impl ObservedTcp {
    pub(crate) fn new(socket: TcpStream) -> Self {
        if !tracing::event_enabled!(target: "bex.net.packet", tracing::Level::TRACE) {
            return Self {
                socket,
                capture: None,
            };
        }
        let span = tracing::Span::current();
        #[cfg(target_vendor = "apple")]
        let probe = {
            use std::os::fd::AsRawFd;
            let probe = Arc::new(Mutex::new(Some(socket.as_raw_fd())));
            let weak = Arc::downgrade(&probe);
            let span = span.clone();
            let dispatcher = tracing::dispatcher::get_default(Clone::clone);
            std::thread::spawn(move || {
                tracing::dispatcher::with_default(&dispatcher, || loop {
                    let Some(probe) = weak.upgrade() else { break };
                    {
                        let fd = probe.lock().unwrap();
                        let Some(fd) = *fd else { break };
                        span.in_scope(|| {
                            if tracing::event_enabled!(target: "bex.net.packet", tracing::Level::TRACE) {
                                sample(fd);
                            }
                        });
                    }
                    drop(probe);
                    std::thread::sleep(std::time::Duration::from_millis(250));
                })
            });
            probe
        };
        #[cfg(not(target_vendor = "apple"))]
        tracing::trace!(target: "bex.net.packet", phase = "relay_tcp_info_unavailable", value = 0u64);
        Self {
            socket,
            capture: Some(Capture {
                read: IoWake::new(&span, "relay_tcp_read_wake"),
                write: IoWake::new(&span, "relay_tcp_write_wake"),
                span,
                read_bytes: 0,
                written_bytes: 0,
                #[cfg(target_vendor = "apple")]
                probe,
            }),
        }
    }
    pub(super) fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }
    pub(super) fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.socket.peer_addr()
    }
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use super::*;
    use std::{
        io::Write,
        time::{Duration, Instant},
    };
    use tracing_subscriber::{layer::SubscriberExt, Layer};

    #[derive(Clone)]
    struct Samples(Arc<Mutex<Vec<(Instant, u64)>>>);
    impl<S: tracing::Subscriber> Layer<S> for Samples {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            #[derive(Default)]
            struct Fields {
                phase: String,
                metric: u64,
                value: u64,
            }
            impl tracing::field::Visit for Fields {
                fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                    if field.name() == "phase" {
                        self.phase = value.into();
                    }
                }
                fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
                    match field.name() {
                        "metric" => self.metric = value,
                        "value" => self.value = value,
                        _ => (),
                    }
                }
                fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
            }
            let mut fields = Fields::default();
            event.record(&mut fields);
            if fields.phase == "relay_tcp_metric" && fields.metric == 6 {
                self.0.lock().unwrap().push((Instant::now(), fields.value));
            }
        }
    }

    #[test]
    fn kernel_receive_is_observed_while_the_tokio_thread_is_stalled() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(75));
            socket.write_all(&[7; 4096]).unwrap();
        });
        let samples = Samples(Default::default());
        tracing::subscriber::with_default(
            tracing_subscriber::registry().with(samples.clone()),
            || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(async {
                        let socket = TcpStream::connect(address).await.unwrap();
                        let mut socket = ObservedTcp::new(socket);
                        let mut bytes = [0; 8];
                        std::future::poll_fn(|cx| {
                            let mut buf = ReadBuf::new(&mut bytes);
                            assert!(Pin::new(&mut socket).poll_read(cx, &mut buf).is_pending());
                            Poll::Ready(())
                        })
                        .await;
                        // Intentional executor stall. The OS and independent sampler must continue.
                        std::thread::sleep(Duration::from_millis(600));
                        let resumed = Instant::now();
                        assert!(
                            samples
                                .0
                                .lock()
                                .unwrap()
                                .iter()
                                .any(|(at, bytes)| *at < resumed && *bytes >= 4096),
                            "kernel receipt must be visible before application reading resumes"
                        );
                        tokio::io::AsyncReadExt::read_exact(&mut socket, &mut bytes)
                            .await
                            .unwrap();
                        assert_eq!(bytes, [7; 8]);
                    });
            },
        );
        server.join().unwrap();
    }
}

#[cfg(target_vendor = "apple")]
impl Drop for ObservedTcp {
    fn drop(&mut self) {
        // Synchronize with getsockopt before socket closure or descriptor reuse.
        if let Some(capture) = &self.capture {
            *capture.probe.lock().unwrap() = None;
        }
    }
}

impl AsyncRead for ObservedTcp {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        let Some(capture) = &mut this.capture else {
            return Pin::new(&mut this.socket).poll_read(cx, buf);
        };
        let _entered = capture.span.enter();
        let waker = capture.read.waker(cx.waker());
        let mut context = Context::from_waker(&waker);
        let before = buf.filled().len();
        let result = Pin::new(&mut this.socket).poll_read(&mut context, buf);
        let bytes = (buf.filled().len() - before) as u64;
        capture.read_bytes += bytes;
        tracing::trace!(target: "bex.net.packet", phase = if result.is_pending() { "relay_tcp_read_pending" } else { "relay_tcp_read" }, value = bytes, offset = capture.read_bytes);
        result
    }
}
impl AsyncWrite for ObservedTcp {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        let Some(capture) = &mut this.capture else {
            return Pin::new(&mut this.socket).poll_write(cx, buf);
        };
        let _entered = capture.span.enter();
        let waker = capture.write.waker(cx.waker());
        let mut context = Context::from_waker(&waker);
        let result = Pin::new(&mut this.socket).poll_write(&mut context, buf);
        let bytes = match &result {
            Poll::Ready(Ok(n)) => *n as u64,
            _ => 0,
        };
        capture.written_bytes += bytes;
        tracing::trace!(target: "bex.net.packet", phase = if result.is_pending() { "relay_tcp_write_pending" } else { "relay_tcp_write" }, value = bytes, offset = capture.written_bytes);
        result
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        let Some(capture) = &mut this.capture else {
            return Pin::new(&mut this.socket).poll_write_vectored(cx, bufs);
        };
        let _entered = capture.span.enter();
        let waker = capture.write.waker(cx.waker());
        let mut context = Context::from_waker(&waker);
        let result = Pin::new(&mut this.socket).poll_write_vectored(&mut context, bufs);
        let bytes = match &result {
            Poll::Ready(Ok(n)) => *n as u64,
            _ => 0,
        };
        capture.written_bytes += bytes;
        tracing::trace!(target: "bex.net.packet", phase = if result.is_pending() { "relay_tcp_write_pending" } else { "relay_tcp_write" }, value = bytes, offset = capture.written_bytes);
        result
    }
    fn is_write_vectored(&self) -> bool {
        self.socket.is_write_vectored()
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_shutdown(cx)
    }
}

#[cfg(target_vendor = "apple")]
fn sample(fd: std::os::fd::RawFd) {
    unsafe extern "C" {
        fn bex_relay_tcp_info(fd: std::ffi::c_int, values: *mut u64) -> std::ffi::c_int;
    }
    let mut values = [0u64; 11];
    // Descriptor lifetime is protected by the caller's mutex. The C shim uses
    // the selected Apple SDK's struct layout and fills exactly 11 counters.
    let error = unsafe { bex_relay_tcp_info(fd, values.as_mut_ptr()) };
    if error != 0 {
        tracing::trace!(target: "bex.net.packet", phase = "relay_tcp_info_unavailable", value = error.unsigned_abs() as u64);
        return;
    }
    for (index, value) in values.into_iter().enumerate() {
        tracing::trace!(target: "bex.net.packet", phase = "relay_tcp_metric", metric = index as u64 + 1, value);
    }
}
