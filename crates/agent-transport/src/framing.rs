use agent_protocol::protocol::*;
use futures_util::StreamExt;
use serde::{Serialize, de::DeserializeOwned};
use std::io;
use tokio_util::codec::{FramedRead, LengthDelimitedCodec};
pub async fn write(send: &mut iroh::endpoint::SendStream, value: impl Serialize) -> io::Result<()> {
    write_frame(send, &encode(value)?).await
}
/// Oversized results fail this request without closing unrelated streams.
pub async fn write_frame(send: &mut iroh::endpoint::SendStream, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message exceeds frame limit",
        ));
    }
    send.write_all(&(bytes.len() as u32).to_be_bytes())
        .await
        .map_err(io::Error::other)?;
    send.write_all(bytes).await.map_err(io::Error::other)
}
struct ReadWake {
    trace: std::sync::Arc<crate::diagnostics::connection::Trace>,
    group: u64,
    stream: u64,
    upstream: std::sync::Mutex<Option<std::task::Waker>>,
}
impl std::task::Wake for ReadWake {
    fn wake(self: std::sync::Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &std::sync::Arc<Self>) {
        let upstream = self.upstream.lock().unwrap().clone();
        if let Some(upstream) = upstream {
            self.trace.record(
                crate::diagnostics::ConnectionPhase::ReadWake,
                self.group,
                self.stream,
                0,
            );
            upstream.wake();
        }
    }
}
struct ReadObservation {
    wake: std::sync::Arc<ReadWake>,
    first: bool,
}
impl Drop for ReadObservation {
    fn drop(&mut self) {
        self.wake.upstream.lock().unwrap().take();
    }
}

#[cfg(test)]
#[test]
fn observed_wake_forwards_and_releases_the_waiting_task_on_drop() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::task::Wake;
    struct Counter(AtomicUsize);
    impl Wake for Counter {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    let counter = Arc::new(Counter(AtomicUsize::new(0)));
    let trace = crate::diagnostics::connection::Trace::new();
    let wake = Arc::new(ReadWake {
        trace: trace.clone(),
        group: 2,
        stream: 4,
        upstream: std::sync::Mutex::new(Some(std::task::Waker::from(counter.clone()))),
    });
    let observation = ReadObservation {
        wake: wake.clone(),
        first: true,
    };
    wake.wake_by_ref();
    assert_eq!(counter.0.load(Ordering::Relaxed), 1);
    assert_eq!(trace.snapshot().events.len(), 1);
    drop(observation);
    assert_eq!(Arc::strong_count(&counter), 1);
    wake.wake_by_ref();
    assert_eq!(counter.0.load(Ordering::Relaxed), 1);
    assert_eq!(trace.snapshot().events.len(), 1);
}

struct ObservedRecv {
    recv: iroh::endpoint::RecvStream,
    observation: Option<ReadObservation>,
}
impl tokio::io::AsyncRead for ObservedRecv {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        use crate::diagnostics::ConnectionPhase as Phase;
        let this = self.get_mut();
        let Some(observation) = this
            .observation
            .as_mut()
            .filter(|value| value.wake.trace.active())
        else {
            return std::pin::Pin::new(&mut this.recv).poll_read(cx, buf);
        };
        let wake = &observation.wake;
        *wake.upstream.lock().unwrap() = Some(cx.waker().clone());
        let waker = std::task::Waker::from(wake.clone());
        let before = buf.filled().len();
        wake.trace
            .record(Phase::ReadPolled, wake.group, wake.stream, 0);
        let result = std::pin::Pin::new(&mut this.recv)
            .poll_read(&mut std::task::Context::from_waker(&waker), buf);
        if result.is_pending() {
            wake.trace
                .record(Phase::ReadPending, wake.group, wake.stream, 0);
        }
        if observation.first && buf.filled().len() > before {
            observation.first = false;
            wake.trace.record(
                Phase::ResponseFirstRead,
                wake.group,
                wake.stream,
                (buf.filled().len() - before) as u64,
            );
        }
        result
    }
}
pub struct Reader(FramedRead<ObservedRecv, LengthDelimitedCodec>);
impl Reader {
    pub fn new(recv: iroh::endpoint::RecvStream) -> Self {
        Self(
            LengthDelimitedCodec::builder()
                .max_frame_length(MAX_FRAME_BYTES)
                .new_read(ObservedRecv {
                    recv,
                    observation: None,
                }),
        )
    }
    pub(crate) fn observed(
        recv: iroh::endpoint::RecvStream,
        trace: std::sync::Arc<crate::diagnostics::connection::Trace>,
        group: u64,
    ) -> Self {
        let mut reader = Self::new(recv);
        let stream = reader.stream_id();
        reader.0.get_mut().observation = Some(ReadObservation {
            wake: std::sync::Arc::new(ReadWake {
                trace,
                group,
                stream,
                upstream: Default::default(),
            }),
            first: true,
        });
        reader
    }
    pub(crate) fn stream_id(&self) -> u64 {
        u64::from(self.0.get_ref().recv.id())
    }
    pub async fn read_frame(&mut self) -> io::Result<Option<tokio_util::bytes::BytesMut>> {
        let frame = self.0.next().await.transpose()?;
        if let Some(observation) = self.0.get_mut().observation.take() {
            observation.wake.trace.record(
                crate::diagnostics::ConnectionPhase::ResponseReceived,
                observation.wake.group,
                self.stream_id(),
                frame.as_ref().map_or(0, |bytes| bytes.len() as u64),
            );
        }
        Ok(frame)
    }
    /// FramedRead retains partial frames if this future loses a select.
    pub async fn read<T: DeserializeOwned>(&mut self) -> io::Result<Option<T>> {
        let Some(bytes) = self.read_frame().await? else {
            return Ok(None);
        };
        decode(&bytes).map(Some)
    }
}
