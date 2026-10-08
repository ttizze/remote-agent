//! Ordered device-frame decoding for the desktop panel.
//!
//! The Host delivers complete access units. JPEG/PNG frames are handed to the
//! panel immediately. H.264/SEMU frames are fed to one long-lived, bounded
//! decoder worker per device screen. The worker owns ffmpeg and all blocking
//! process I/O; the GPUI render path only queues input and drains completed
//! images.

use agent_core::view::device::DeviceVideoFrameView;
use host_daemon::device_stream::jpeg_bounds;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MAX_H264_BYTES: usize = 8 * 1024 * 1024;
const MAX_WORKER_QUEUE: usize = 8;
const MAX_WORKER_OUTPUT: usize = 8;
const WORKER_STOP_TIMEOUT: Duration = Duration::from_secs(5);

type StreamKey = (String, String, String, u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeviceImageFormat {
    Jpeg,
    Png,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DecodedDeviceImage {
    pub host_id: String,
    pub sequence: u64,
    pub device_id: String,
    pub screen_id: u8,
    pub format: DeviceImageFormat,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
enum WorkerCommand {
    Configure(Vec<u8>),
    Frame { sequence: u64, payload: Vec<u8> },
    Stop,
}

#[derive(Debug)]
enum WorkerOutput {
    Image { sequence: u64, bytes: Vec<u8> },
    Error(String),
}

#[derive(Debug)]
enum ReaderEvent {
    Image(Vec<u8>),
    Error(String),
    Eof,
}

#[derive(Debug)]
enum WriterCommand {
    Bytes(Vec<u8>),
    Stop,
}

/// A non-blocking handle to the process owned by a decoder thread. Shutdown
/// marks the worker cancelled, asks it to close stdin, and joins it on a
/// short-lived reaper thread rather than making the GPUI path wait on a child
/// process or a full pipe.
struct DecoderWorker {
    commands: SyncSender<WorkerCommand>,
    output: Receiver<WorkerOutput>,
    cancelled: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl DecoderWorker {
    fn new() -> Self {
        let (commands, command_rx) = mpsc::sync_channel(MAX_WORKER_QUEUE);
        let (output_tx, output) = mpsc::sync_channel(MAX_WORKER_OUTPUT);
        let cancelled = Arc::new(AtomicBool::new(false));
        let thread_cancelled = Arc::clone(&cancelled);
        let join = thread::Builder::new()
            .name("device-video-decoder".into())
            .spawn(move || run_decoder_worker(command_rx, output_tx, thread_cancelled))
            .ok();
        Self {
            commands,
            output,
            cancelled,
            join,
        }
    }

    fn try_send(&self, command: WorkerCommand) -> Result<(), String> {
        match self.commands.try_send(command) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                Err("Device video decoder is still processing earlier frames".into())
            }
            Err(TrySendError::Disconnected(_)) => {
                Err("Device video decoder stopped unexpectedly".into())
            }
        }
    }

    fn drain(&self, output: &mut Vec<WorkerOutput>) {
        loop {
            match self.output.try_recv() {
                Ok(item) => output.push(item),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }

    fn shutdown(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        let _ = self.commands.try_send(WorkerCommand::Stop);
        let Some(join) = self.join.take() else { return };
        // The worker kills a child that does not finish by its own deadline.
        // Reaping off the UI thread guarantees process ownership is still
        // released without making panel reset/render block.
        let _ = thread::Builder::new()
            .name("device-video-decoder-reaper".into())
            .spawn(move || {
                let _ = join.join();
            });
    }
}

impl Drop for DecoderWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(super) struct DeviceVideoDecoder {
    latest_sequence: BTreeMap<StreamKey, u64>,
    awaiting_keyframe: BTreeSet<StreamKey>,
    descriptions: BTreeMap<StreamKey, Vec<u8>>,
    access_units: BTreeMap<StreamKey, Vec<u8>>,
    decoded_sequence: BTreeMap<StreamKey, u64>,
    workers: BTreeMap<StreamKey, DecoderWorker>,
    error: Option<String>,
}

impl Default for DeviceVideoDecoder {
    fn default() -> Self {
        Self {
            latest_sequence: BTreeMap::new(),
            awaiting_keyframe: BTreeSet::new(),
            descriptions: BTreeMap::new(),
            access_units: BTreeMap::new(),
            decoded_sequence: BTreeMap::new(),
            workers: BTreeMap::new(),
            error: None,
        }
    }
}

impl DeviceVideoDecoder {
    pub(super) fn reset(&mut self) {
        for worker in self.workers.values_mut() {
            worker.shutdown();
        }
        self.workers.clear();
        self.latest_sequence.clear();
        self.awaiting_keyframe.clear();
        self.descriptions.clear();
        self.access_units.clear();
        self.decoded_sequence.clear();
        self.error = None;
    }

    pub(super) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Queue live frames and return direct frames plus any worker results that
    /// completed since the previous panel update. This method never waits on
    /// a child, reads a pipe, or allocates beyond the worker's bounded queues.
    pub(super) fn push(&mut self, frames: &[DeviceVideoFrameView]) -> Vec<DecodedDeviceImage> {
        let mut decoded = Vec::new();
        self.drain_workers(&mut decoded);
        let mut ordered = frames.to_vec();
        ordered.sort_by_key(|frame| {
            (
                frame.thread_id.clone(),
                frame.host_id.clone(),
                frame.device_id.clone(),
                frame.screen_id.unwrap_or(0),
                frame.sequence,
            )
        });
        for frame in ordered {
            self.push_live(frame, &mut decoded);
        }
        self.drain_workers(&mut decoded);
        decoded
    }

    fn push_live(&mut self, frame: DeviceVideoFrameView, decoded: &mut Vec<DecodedDeviceImage>) {
        let screen = frame.screen_id.unwrap_or(0);
        let stream_key = (
            frame.thread_id.clone(),
            frame.host_id.clone(),
            frame.device_id.clone(),
            screen,
        );
        let previous = self.latest_sequence.get(&stream_key).copied();
        if previous.is_some_and(|sequence| frame.sequence <= sequence) {
            return;
        }
        if previous.is_some_and(|sequence| frame.sequence > sequence.saturating_add(1)) {
            self.resync_stream(&stream_key);
        }
        let new_stream = self
            .latest_sequence
            .insert(stream_key.clone(), frame.sequence)
            .is_none();
        match frame.encoding.as_str() {
            "jpeg" | "mjpeg" => {
                if let Some((start, end)) = jpeg_bounds(&frame.payload) {
                    self.decoded_sequence
                        .insert(stream_key, frame.sequence);
                    decoded.push(DecodedDeviceImage {
                        host_id: frame.host_id,
                        sequence: frame.sequence,
                        device_id: frame.device_id,
                        screen_id: screen,
                        format: DeviceImageFormat::Jpeg,
                        bytes: frame.payload[start..end].to_vec(),
                    });
                    self.error = None;
                } else {
                    self.error = Some("Device stream returned an invalid JPEG frame".into());
                }
            }
            "png" => {
                if !frame.payload.is_empty() {
                    self.decoded_sequence
                        .insert(stream_key, frame.sequence);
                    decoded.push(DecodedDeviceImage {
                        host_id: frame.host_id,
                        sequence: frame.sequence,
                        device_id: frame.device_id,
                        screen_id: screen,
                        format: DeviceImageFormat::Png,
                        bytes: frame.payload,
                    });
                    self.error = None;
                }
            }
            "avcc-description" => {
                let description = description_to_annex_b(&frame.payload);
                if description.is_empty() {
                    self.error = Some("Device stream returned an invalid H.264 description".into());
                } else {
                    self.descriptions.insert(stream_key.clone(), description);
                    self.resync_stream(&stream_key);
                }
            }
            "h264" | "semu" => {
                if new_stream {
                    self.awaiting_keyframe.insert(stream_key.clone());
                }
                if self.awaiting_keyframe.contains(&stream_key) && !frame.keyframe {
                    return;
                }
                let access_unit = to_annex_b(&frame.payload);
                if access_unit.is_empty() {
                    self.error = Some("Device stream returned an invalid H.264 access unit".into());
                    self.resync_stream(&stream_key);
                    return;
                }
                if frame.keyframe {
                    let mut stream = self
                        .descriptions
                        .get(&stream_key)
                        .cloned()
                        .unwrap_or_default();
                    stream.extend_from_slice(&access_unit);
                    self.access_units.insert(stream_key.clone(), stream);
                    self.awaiting_keyframe.remove(&stream_key);
                    self.restart_worker(&stream_key);
                } else {
                    let stream = self.access_units.entry(stream_key.clone()).or_default();
                    stream.extend_from_slice(&access_unit);
                }
                let oversized = self
                    .access_units
                    .get(&stream_key)
                    .is_some_and(|stream| stream.len() > MAX_H264_BYTES);
                if oversized {
                    self.error = Some("Device video decoder dropped an oversized access-unit buffer".into());
                    self.resync_stream(&stream_key);
                    return;
                }
                let send_result = self
                    .worker_for(&stream_key)
                    .map(|worker| {
                        worker.try_send(WorkerCommand::Frame {
                            sequence: frame.sequence,
                            payload: access_unit,
                        })
                    })
                    .unwrap_or_else(|| Err("Device video decoder could not start".into()));
                if let Err(error) = send_result {
                    self.error = Some(error);
                    self.resync_stream(&stream_key);
                    return;
                }
                self.drain_worker(&stream_key, decoded);
            }
            _ => {
                self.error = Some(format!(
                    "Device stream encoding is unsupported: {}",
                    frame.encoding
                ));
            }
        }
    }

    fn worker_for(&mut self, stream_key: &StreamKey) -> Option<&DecoderWorker> {
        if !self.workers.contains_key(stream_key) {
            let worker = DecoderWorker::new();
            let description = self
                .descriptions
                .get(stream_key)
                .cloned()
                .unwrap_or_default();
            if let Err(error) = worker.try_send(WorkerCommand::Configure(description)) {
                self.error = Some(error);
                return None;
            }
            self.workers.insert(stream_key.clone(), worker);
        }
        self.workers.get(stream_key)
    }

    fn drain_workers(&mut self, decoded: &mut Vec<DecodedDeviceImage>) {
        let keys = self.workers.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            self.drain_worker(&key, decoded);
        }
    }

    fn drain_worker(&mut self, key: &StreamKey, decoded: &mut Vec<DecodedDeviceImage>) {
        let mut output = Vec::new();
        let Some(worker) = self.workers.get(key) else { return };
        worker.drain(&mut output);
        let mut failed = false;
        for item in output {
            match item {
                WorkerOutput::Image { sequence, bytes } => {
                    let Some((start, end)) = jpeg_bounds(&bytes) else {
                        self.error = Some("Device video decoder returned an invalid image".into());
                        failed = true;
                        continue;
                    };
                    if self
                        .decoded_sequence
                        .get(key)
                        .is_some_and(|previous| sequence <= *previous)
                    {
                        continue;
                    }
                    self.decoded_sequence.insert(key.clone(), sequence);
                    decoded.push(DecodedDeviceImage {
                        host_id: key.1.clone(),
                        sequence,
                        device_id: key.2.clone(),
                        screen_id: key.3,
                        format: DeviceImageFormat::Jpeg,
                        bytes: bytes[start..end].to_vec(),
                    });
                    self.error = None;
                }
                WorkerOutput::Error(error) => {
                    self.error = Some(error);
                    failed = true;
                }
            }
        }
        if failed {
            self.resync_stream(key);
        }
    }

    fn restart_worker(&mut self, key: &StreamKey) {
        if let Some(mut worker) = self.workers.remove(key) {
            worker.shutdown();
        }
    }

    fn resync_stream(&mut self, key: &StreamKey) {
        self.access_units.remove(key);
        self.awaiting_keyframe.insert(key.clone());
        self.restart_worker(key);
    }

    fn push_with<F>(
        &mut self,
        frames: &[DeviceVideoFrameView],
        mut decode_h264: F,
    ) -> Vec<DecodedDeviceImage>
    where
        F: FnMut(&[u8]) -> Result<Vec<u8>, String>,
    {
        let mut decoded = Vec::new();
        let mut ordered = frames.to_vec();
        ordered.sort_by_key(|frame| {
            (
                frame.screen_id.unwrap_or(0),
                frame.sequence,
            )
        });
        for frame in ordered {
            let screen = frame.screen_id.unwrap_or(0);
            let stream_key = (
                frame.thread_id.clone(),
                frame.host_id.clone(),
                frame.device_id.clone(),
                screen,
            );
            let previous = self.latest_sequence.get(&stream_key).copied();
            if previous.is_some_and(|sequence| frame.sequence <= sequence) {
                continue;
            }
            if previous.is_some_and(|sequence| frame.sequence > sequence.saturating_add(1)) {
                self.access_units.remove(&stream_key);
                self.awaiting_keyframe.insert(stream_key.clone());
            }
            let new_stream = self
                .latest_sequence
                .insert(stream_key.clone(), frame.sequence)
                .is_none();
            match frame.encoding.as_str() {
                "jpeg" | "mjpeg" => {
                    if let Some((start, end)) = jpeg_bounds(&frame.payload) {
                        decoded.push(DecodedDeviceImage {
                            host_id: frame.host_id,
                            sequence: frame.sequence,
                            device_id: frame.device_id,
                            screen_id: screen,
                            format: DeviceImageFormat::Jpeg,
                            bytes: frame.payload[start..end].to_vec(),
                        });
                    }
                }
                "png" => {
                    if !frame.payload.is_empty() {
                        decoded.push(DecodedDeviceImage {
                            host_id: frame.host_id,
                            sequence: frame.sequence,
                            device_id: frame.device_id,
                            screen_id: screen,
                            format: DeviceImageFormat::Png,
                            bytes: frame.payload,
                        });
                    }
                }
                "avcc-description" => {
                    self.descriptions
                        .insert(stream_key.clone(), description_to_annex_b(&frame.payload));
                    self.access_units.remove(&stream_key);
                    self.awaiting_keyframe.insert(stream_key);
                }
                "h264" | "semu" => {
                    if new_stream {
                        self.awaiting_keyframe.insert(stream_key.clone());
                    }
                    if self.awaiting_keyframe.contains(&stream_key) && !frame.keyframe {
                        continue;
                    }
                    if frame.keyframe {
                        let mut stream = self
                            .descriptions
                            .get(&stream_key)
                            .cloned()
                            .unwrap_or_default();
                        stream.extend_from_slice(&to_annex_b(&frame.payload));
                        self.access_units.insert(stream_key.clone(), stream);
                        self.awaiting_keyframe.remove(&stream_key);
                    } else {
                        let stream = self.access_units.entry(stream_key.clone()).or_default();
                        stream.extend_from_slice(&to_annex_b(&frame.payload));
                    }
                    let stream = self.access_units.get(&stream_key).cloned().unwrap_or_default();
                    if stream.len() > MAX_H264_BYTES {
                        self.access_units.remove(&stream_key);
                        self.awaiting_keyframe.insert(stream_key);
                        self.error = Some("Device video decoder dropped an oversized access-unit buffer".into());
                        continue;
                    }
                    match decode_h264(&stream) {
                        Ok(bytes) => {
                            if let Some((start, end)) = last_jpeg_bounds(&bytes) {
                                self.error = None;
                                decoded.push(DecodedDeviceImage {
                                    host_id: frame.host_id,
                                    sequence: frame.sequence,
                                    device_id: frame.device_id,
                                    screen_id: screen,
                                    format: DeviceImageFormat::Jpeg,
                                    bytes: bytes[start..end].to_vec(),
                                });
                            } else {
                                self.error = Some("Device video decoder returned an invalid image".into());
                            }
                        }
                        Err(error) => {
                            self.access_units.remove(&stream_key);
                            self.awaiting_keyframe.insert(stream_key);
                            self.error = Some(error);
                        }
                    }
                }
                _ => self.error = Some(format!(
                    "Device stream encoding is unsupported: {}",
                    frame.encoding
                )),
            }
        }
        decoded
    }
}

impl Drop for DeviceVideoDecoder {
    fn drop(&mut self) {
        for worker in self.workers.values_mut() {
            worker.shutdown();
        }
    }
}

fn ffmpeg_executable() -> OsString {
    if let Some(path) = std::env::var_os("AGENT_FFMPEG_EXECUTABLE") {
        if !path.is_empty() {
            return path;
        }
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(parent) = current.parent() {
            for name in ["ffmpeg", "ffmpeg.exe"] {
                let candidate = parent.join(name);
                if candidate.is_file() {
                    return candidate.into_os_string();
                }
            }
        }
    }
    OsString::from("ffmpeg")
}

fn run_decoder_worker(
    command_rx: Receiver<WorkerCommand>,
    output_tx: SyncSender<WorkerOutput>,
    cancelled: Arc<AtomicBool>,
) {
    let executable = ffmpeg_executable();
    let mut child = match Command::new(executable)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "h264",
            "-i",
            "pipe:0",
            "-an",
            "-sn",
            "-dn",
            "-f",
            "mjpeg",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            let _ = output_tx.try_send(WorkerOutput::Error(
                "Device video decoder runtime is unavailable".into(),
            ));
            return;
        }
    };
    if child.try_wait().ok().flatten().is_some() {
        let _ = output_tx.try_send(WorkerOutput::Error(
            "Device video decoder runtime exited during startup".into(),
        ));
        return;
    }
    let Some(stdin) = child.stdin.take() else {
        let _ = output_tx.try_send(WorkerOutput::Error(
            "Device video decoder could not open its input".into(),
        ));
        let _ = child.kill();
        let _ = child.wait();
        return;
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = output_tx.try_send(WorkerOutput::Error(
            "Device video decoder could not open its output".into(),
        ));
        let _ = child.kill();
        let _ = child.wait();
        return;
    };
    let stderr = child.stderr.take();
    let (reader_rx, reader_join) = spawn_jpeg_reader(stdout, Arc::clone(&cancelled));
    let stderr_join = stderr.map(|stderr| spawn_stderr_drainer(stderr, Arc::clone(&cancelled)));
    let (writer_tx, writer_rx) = mpsc::sync_channel(MAX_WORKER_QUEUE);
    let writer_join = spawn_decoder_writer(stdin, writer_rx, Arc::clone(&cancelled), output_tx.clone());
    let mut pending = VecDeque::new();
    let mut stopping = false;
    let mut stop_started = None;

    loop {
        if drain_reader(&reader_rx, &mut pending, &output_tx) {
            stopping = true;
            stop_started.get_or_insert_with(Instant::now);
        }
        if cancelled.load(Ordering::Acquire) {
            stopping = true;
            stop_started.get_or_insert_with(Instant::now);
            let _ = child.kill();
        }
        if !stopping {
            match command_rx.recv_timeout(Duration::from_millis(25)) {
                Ok(WorkerCommand::Configure(bytes)) => {
                    if writer_tx.try_send(WriterCommand::Bytes(bytes)).is_err() {
                        let _ = output_tx.try_send(WorkerOutput::Error(
                            "Device video decoder input queue is full".into(),
                        ));
                        stopping = true;
                        stop_started.get_or_insert_with(Instant::now);
                    }
                }
                Ok(WorkerCommand::Frame { sequence, payload }) => {
                    pending.push_back(sequence);
                    if writer_tx.try_send(WriterCommand::Bytes(payload)).is_err() {
                        pending.pop_back();
                        let _ = output_tx.try_send(WorkerOutput::Error(
                            "Device video decoder input queue is full".into(),
                        ));
                        stopping = true;
                        stop_started.get_or_insert_with(Instant::now);
                    }
                }
                Ok(WorkerCommand::Stop) => {
                    stopping = true;
                    stop_started.get_or_insert_with(Instant::now);
                    let _ = writer_tx.try_send(WriterCommand::Stop);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    stopping = true;
                    stop_started.get_or_insert_with(Instant::now);
                    let _ = writer_tx.try_send(WriterCommand::Stop);
                }
            }
        }
        if child.try_wait().ok().flatten().is_some() {
            break;
        }
        if stopping {
            if stop_started.is_some_and(|started| started.elapsed() >= WORKER_STOP_TIMEOUT) {
                let _ = child.kill();
                break;
            }
            if cancelled.load(Ordering::Acquire) {
                let _ = child.kill();
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    drop(writer_tx);
    let _ = writer_join.join();
    let _ = reader_join.join();
    if let Some(join) = stderr_join {
        let _ = join.join();
    }
    let _ = drain_reader(&reader_rx, &mut pending, &output_tx);
}

fn spawn_decoder_writer(
    mut stdin: ChildStdin,
    commands: Receiver<WriterCommand>,
    cancelled: Arc<AtomicBool>,
    output: SyncSender<WorkerOutput>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("device-video-decoder-writer".into())
        .spawn(move || {
            while let Ok(command) = commands.recv() {
                match command {
                    WriterCommand::Bytes(bytes) => {
                        if cancelled.load(Ordering::Acquire) {
                            break;
                        }
                        if stdin.write_all(&bytes).is_err() {
                            let _ = output.try_send(WorkerOutput::Error(
                                "Device video decoder could not feed the access unit".into(),
                            ));
                            break;
                        }
                    }
                    WriterCommand::Stop => break,
                }
            }
        })
        .unwrap_or_else(|_| thread::spawn(|| {}))
}

fn spawn_jpeg_reader(
    stdout: ChildStdout,
    cancelled: Arc<AtomicBool>,
) -> (Receiver<ReaderEvent>, JoinHandle<()>) {
    let (events, receiver) = mpsc::sync_channel(MAX_WORKER_OUTPUT);
    let join = thread::Builder::new()
        .name("device-video-decoder-reader".into())
        .spawn(move || {
            let mut stdout = stdout;
            let mut buffer = Vec::new();
            let mut chunk = [0_u8; 64 * 1024];
            loop {
                if cancelled.load(Ordering::Acquire) {
                    return;
                }
                let read = match stdout.read(&mut chunk) {
                    Ok(0) => {
                        let _ = events.try_send(ReaderEvent::Eof);
                        return;
                    }
                    Ok(read) => read,
                    Err(_) => {
                        let _ = events.try_send(ReaderEvent::Error(
                            "Device video decoder output could not be read".into(),
                        ));
                        return;
                    }
                };
                buffer.extend_from_slice(&chunk[..read]);
                if buffer.len() > MAX_H264_BYTES {
                    let _ = events.try_send(ReaderEvent::Error(
                        "Device video decoder returned an oversized image stream".into(),
                    ));
                    return;
                }
                while let Some((start, end)) = jpeg_bounds(&buffer) {
                    let image = buffer[start..end].to_vec();
                    buffer.drain(..end);
                    if events.try_send(ReaderEvent::Image(image)).is_err() {
                        let _ = events.try_send(ReaderEvent::Error(
                            "Device video decoder output queue is full".into(),
                        ));
                        return;
                    }
                }
            }
        })
        .unwrap_or_else(|_| thread::spawn(|| {}));
    (receiver, join)
}

fn spawn_stderr_drainer(stderr: std::process::ChildStderr, cancelled: Arc<AtomicBool>) -> JoinHandle<()> {
    thread::Builder::new()
        .name("device-video-decoder-stderr".into())
        .spawn(move || {
            let mut stderr = stderr;
            let mut chunk = [0_u8; 4096];
            while !cancelled.load(Ordering::Acquire) {
                match stderr.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        })
        .unwrap_or_else(|_| thread::spawn(|| {}))
}

fn drain_reader(
    reader: &Receiver<ReaderEvent>,
    pending: &mut VecDeque<u64>,
    output: &SyncSender<WorkerOutput>,
) -> bool {
    let mut ended = false;
    loop {
        match reader.try_recv() {
            Ok(ReaderEvent::Image(bytes)) => {
                if let Some(sequence) = pending.pop_front() {
                    if output
                        .try_send(WorkerOutput::Image { sequence, bytes })
                        .is_err()
                    {
                        ended = true;
                        break;
                    }
                }
            }
            Ok(ReaderEvent::Error(error)) => {
                let _ = output.try_send(WorkerOutput::Error(error));
                ended = true;
                break;
            }
            Ok(ReaderEvent::Eof) => {
                ended = true;
                break;
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
    ended
}

fn to_annex_b(bytes: &[u8]) -> Vec<u8> {
    if bytes.starts_with(&[0, 0, 1]) || bytes.starts_with(&[0, 0, 0, 1]) {
        return bytes.to_vec();
    }
    let mut result = Vec::with_capacity(bytes.len().saturating_add(16));
    let mut offset = 0;
    while offset.saturating_add(4) <= bytes.len() {
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        if length == 0 || offset.saturating_add(length) > bytes.len() {
            return prefixed_annex_b(bytes);
        }
        result.extend_from_slice(&[0, 0, 0, 1]);
        result.extend_from_slice(&bytes[offset..offset + length]);
        offset += length;
    }
    if offset != bytes.len() || result.is_empty() {
        prefixed_annex_b(bytes)
    } else {
        result
    }
}

fn prefixed_annex_b(bytes: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(bytes.len().saturating_add(4));
    result.extend_from_slice(&[0, 0, 0, 1]);
    result.extend_from_slice(bytes);
    result
}

fn description_to_annex_b(bytes: &[u8]) -> Vec<u8> {
    if bytes.starts_with(&[0, 0, 1]) || bytes.starts_with(&[0, 0, 0, 1]) {
        return bytes.to_vec();
    }
    if bytes.len() < 7 || bytes[0] != 1 {
        return Vec::new();
    }
    let mut offset = 5;
    let sps_count = (bytes[offset] & 0x1f) as usize;
    offset += 1;
    let mut result = Vec::new();
    for _ in 0..sps_count {
        let Some(length_bytes) = bytes.get(offset..offset + 2) else { return Vec::new() };
        let length = u16::from_be_bytes([length_bytes[0], length_bytes[1]]) as usize;
        offset += 2;
        let Some(sps) = bytes.get(offset..offset + length) else { return Vec::new() };
        result.extend_from_slice(&[0, 0, 0, 1]);
        result.extend_from_slice(sps);
        offset += length;
    }
    let Some(&pps_count) = bytes.get(offset) else { return result };
    offset += 1;
    for _ in 0..pps_count as usize {
        let Some(length_bytes) = bytes.get(offset..offset + 2) else { return Vec::new() };
        let length = u16::from_be_bytes([length_bytes[0], length_bytes[1]]) as usize;
        offset += 2;
        let Some(pps) = bytes.get(offset..offset + length) else { return Vec::new() };
        result.extend_from_slice(&[0, 0, 0, 1]);
        result.extend_from_slice(pps);
        offset += length;
    }
    result
}

fn last_jpeg_bounds(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut offset = 0;
    let mut last = None;
    while offset < bytes.len() {
        let (start, end) = jpeg_bounds(&bytes[offset..])?;
        last = Some((offset + start, offset + end));
        offset += end;
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(sequence: u64, encoding: &str, payload: Vec<u8>, keyframe: bool) -> DeviceVideoFrameView {
        DeviceVideoFrameView {
            thread_id: "thread".into(),
            host_id: "host".into(),
            device_id: "device".into(),
            platform: "android".into(),
            payload,
            encoding: encoding.into(),
            width: 2,
            height: 2,
            sequence,
            timestamp_us: Some(sequence),
            keyframe,
            screen_id: Some(1),
        }
    }

    #[test]
    fn ordered_decoder_waits_for_a_keyframe_and_keeps_deltas() {
        let mut decoder = DeviceVideoDecoder::default();
        let mut calls = Vec::new();
        let image = decoder.push_with(
            &[
                frame(2, "h264", vec![0, 0, 1, 0x41, 2], false),
                frame(1, "h264", vec![0, 0, 1, 0x65, 1], true),
                frame(3, "h264", vec![0, 0, 1, 0x41, 3], false),
            ],
            |bytes| {
                calls.push(bytes.to_vec());
                Ok(vec![0xff, 0xd8, 1, 0xff, 0xd9])
            },
        );
        assert_eq!(image.last().map(|image| image.sequence), Some(3));
        assert_eq!(calls.len(), 2);
        assert!(calls[1].windows(2).any(|window| window == [0x41, 3]));
    }

    #[test]
    fn new_stream_deltas_are_ignored_until_a_keyframe() {
        let mut decoder = DeviceVideoDecoder::default();
        let mut calls = 0;
        let images = decoder.push_with(
            &[frame(1, "h264", vec![0, 0, 1, 0x41], false)],
            |_| {
                calls += 1;
                Ok(vec![0xff, 0xd8, 1, 0xff, 0xd9])
            },
        );
        assert!(images.is_empty());
        assert_eq!(calls, 0);
        let images = decoder.push_with(
            &[frame(2, "h264", vec![0, 0, 1, 0x65], true)],
            |_| {
                calls += 1;
                Ok(vec![0xff, 0xd8, 1, 0xff, 0xd9])
            },
        );
        assert_eq!(images.len(), 1);
        assert_eq!(calls, 1);
    }

    #[test]
    fn sequence_gap_waits_for_a_new_keyframe() {
        let mut decoder = DeviceVideoDecoder::default();
        let mut calls = 0;
        let decode = |_| {
            calls += 1;
            Ok(vec![0xff, 0xd8, 1, 0xff, 0xd9])
        };
        assert_eq!(decoder.push_with(&[frame(1, "h264", vec![0, 0, 1, 0x65], true)], decode).len(), 1);
        assert!(decoder.push_with(&[frame(3, "h264", vec![0, 0, 1, 0x41], false)], |_| Ok(Vec::new())).is_empty());
        assert_eq!(decoder.push_with(&[frame(4, "h264", vec![0, 0, 1, 0x65], true)], |_| Ok(vec![0xff, 0xd8, 1, 0xff, 0xd9])).len(), 1);
        assert_eq!(calls, 1);
    }

    #[test]
    fn avcc_description_is_retained_for_later_keyframes() {
        let description = [1, 0x64, 0, 0x1f, 0xff, 0xe1, 0, 2, 0x67, 1, 1, 0, 2, 0x68, 2];
        assert_eq!(description_to_annex_b(&description), vec![
            0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2,
        ]);
        let mut decoder = DeviceVideoDecoder::default();
        decoder.push_with(&[frame(1, "avcc-description", description.to_vec(), false)], |_| unreachable!());
        let mut calls = Vec::new();
        let _ = decoder.push_with(&[frame(2, "h264", vec![0, 0, 1, 0x65], true)], |bytes| {
            calls.push(bytes.to_vec());
            Ok(vec![0xff, 0xd8, 1, 0xff, 0xd9])
        });
        assert!(calls[0].starts_with(&[0, 0, 0, 1, 0x67, 1]));
    }

    #[test]
    fn oversized_stream_resets_until_next_keyframe() {
        let mut decoder = DeviceVideoDecoder::default();
        let huge = vec![0; MAX_H264_BYTES];
        let frame = frame(1, "h264", huge, true);
        assert!(decoder.push_with(&[frame], |_| Ok(Vec::new())).is_empty());
        assert!(decoder.awaiting_keyframe.contains(&(
            "thread".into(),
            "host".into(),
            "device".into(),
            1,
        )));
    }

    #[test]
    fn multiple_decoder_images_choose_the_newest_image() {
        let bytes = [0xff, 0xd8, 1, 0xff, 0xd9, 0xff, 0xd8, 2, 0xff, 0xd9];
        assert_eq!(last_jpeg_bounds(&bytes).map(|bounds| &bytes[bounds.0..bounds.1]), Some(&bytes[5..10]));
    }

    #[test]
    fn independent_devices_keep_independent_sequence_and_decoder_state() {
        let mut decoder = DeviceVideoDecoder::default();
        let mut other = frame(1, "jpeg", vec![0xff, 0xd8, 2, 0xff, 0xd9], true);
        other.host_id = "other-host".into();
        other.device_id = "other-device".into();
        let images = decoder.push_with(
            &[frame(1, "jpeg", vec![0xff, 0xd8, 1, 0xff, 0xd9], true), other],
            |_| unreachable!("JPEG streams do not invoke H.264 decoding"),
        );
        assert_eq!(images.len(), 2);
    }
}
