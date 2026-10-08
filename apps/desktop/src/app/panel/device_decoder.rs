//! Ordered device-frame decoding for the desktop panel.
//!
//! The Host delivers complete access units. JPEG/PNG frames are handed to the
//! panel immediately. H.264/SEMU frames are fed to one long-lived, bounded
//! decoder worker per device screen. The worker owns ffmpeg and all blocking
//! process I/O; the GPUI render path only queues input and drains completed
//! images.

use agent_core::view::device::DeviceVideoFrameView;
use host_daemon::jpeg_bounds;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{Read, Write};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MAX_ACCESS_UNIT_BYTES: usize = 8 * 1024 * 1024;
const MAX_DECODED_IMAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_WORKER_QUEUE: usize = 8;
const MAX_WORKER_OUTPUT: usize = 8;
const WORKER_STOP_TIMEOUT: Duration = Duration::from_secs(5);

type StreamKey = (String, String, String, String, u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SequenceDisposition {
    FirstOrContiguous,
    Gap,
    Duplicate,
}

fn sequence_disposition(previous: Option<u64>, sequence: u64) -> SequenceDisposition {
    match previous {
        None => SequenceDisposition::FirstOrContiguous,
        Some(previous) if sequence <= previous => SequenceDisposition::Duplicate,
        Some(previous) if sequence > previous.saturating_add(1) => SequenceDisposition::Gap,
        Some(_) => SequenceDisposition::FirstOrContiguous,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeviceImageFormat {
    Jpeg,
    Png,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DecodedDeviceImage {
    pub host_id: String,
    pub session_epoch: String,
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
    fn new() -> Result<Self, String> {
        let (commands, command_rx) = mpsc::sync_channel(MAX_WORKER_QUEUE);
        let (output_tx, output) = mpsc::sync_channel(MAX_WORKER_OUTPUT);
        let cancelled = Arc::new(AtomicBool::new(false));
        let thread_cancelled = Arc::clone(&cancelled);
        let join = thread::Builder::new()
            .name("device-video-decoder".into())
            .spawn(move || run_decoder_worker(command_rx, output_tx, thread_cancelled))
            .map_err(|error| format!("Device video decoder worker could not start: {error}"))?;
        Ok(Self {
            commands,
            output,
            cancelled,
            join: Some(join),
        })
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
        self.decoded_sequence.clear();
        self.error = None;
    }

    /// Drop decoder processes and buffered stream state whose session was
    /// closed or reopened. A session epoch is part of every stream key, so a
    /// late frame from the previous capture must never keep its worker alive
    /// or remain visible after a reconnect.
    pub(super) fn retain_sessions(&mut self, active: &BTreeSet<(String, String, String, String)>) {
        let mut keys = BTreeSet::new();
        keys.extend(self.workers.keys().cloned());
        keys.extend(self.latest_sequence.keys().cloned());
        keys.extend(self.awaiting_keyframe.iter().cloned());
        keys.extend(self.descriptions.keys().cloned());
        keys.extend(self.decoded_sequence.keys().cloned());
        let stale = keys
            .into_iter()
            .filter(|key| {
                !active.contains(&(key.0.clone(), key.1.clone(), key.2.clone(), key.3.clone()))
            })
            .collect::<Vec<_>>();
        for key in stale {
            if let Some(mut worker) = self.workers.remove(&key) {
                worker.shutdown();
            }
            self.latest_sequence.remove(&key);
            self.awaiting_keyframe.remove(&key);
            self.descriptions.remove(&key);
            self.decoded_sequence.remove(&key);
        }
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
            frame.session_epoch.clone(),
            screen,
        );
        let previous = self.latest_sequence.get(&stream_key).copied();
        match sequence_disposition(previous, frame.sequence) {
            SequenceDisposition::Duplicate => return,
            SequenceDisposition::Gap => self.resync_stream(&stream_key),
            SequenceDisposition::FirstOrContiguous => {}
        }
        let new_stream = self
            .latest_sequence
            .insert(stream_key.clone(), frame.sequence)
            .is_none();
        match frame.encoding.as_str() {
            "jpeg" | "mjpeg" => {
                if frame.payload.len() > MAX_DECODED_IMAGE_BYTES {
                    self.error = Some("Device stream returned an oversized JPEG frame".into());
                } else if let Some((start, end)) = jpeg_bounds(&frame.payload) {
                    self.decoded_sequence.insert(stream_key, frame.sequence);
                    decoded.push(DecodedDeviceImage {
                        host_id: frame.host_id,
                        session_epoch: frame.session_epoch,
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
                if !frame.payload.is_empty() && frame.payload.len() <= MAX_DECODED_IMAGE_BYTES {
                    self.decoded_sequence.insert(stream_key, frame.sequence);
                    decoded.push(DecodedDeviceImage {
                        host_id: frame.host_id,
                        session_epoch: frame.session_epoch,
                        sequence: frame.sequence,
                        device_id: frame.device_id,
                        screen_id: screen,
                        format: DeviceImageFormat::Png,
                        bytes: frame.payload,
                    });
                    self.error = None;
                } else if frame.payload.len() > MAX_DECODED_IMAGE_BYTES {
                    self.error = Some("Device stream returned an oversized PNG frame".into());
                }
            }
            "avcc-description" => {
                let description = if frame.payload.len() <= MAX_ACCESS_UNIT_BYTES {
                    description_to_annex_b(&frame.payload)
                } else {
                    Vec::new()
                };
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
                if frame.payload.len() > MAX_ACCESS_UNIT_BYTES {
                    self.error =
                        Some("Device video decoder dropped an oversized access unit".into());
                    self.resync_stream(&stream_key);
                    return;
                }
                let access_unit = to_annex_b(&frame.payload);
                if access_unit.is_empty() {
                    self.error = Some("Device stream returned an invalid H.264 access unit".into());
                    self.resync_stream(&stream_key);
                    return;
                }
                if frame.keyframe {
                    self.awaiting_keyframe.remove(&stream_key);
                }
                if access_unit.len() > MAX_ACCESS_UNIT_BYTES {
                    self.error =
                        Some("Device video decoder dropped an oversized access unit".into());
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
            let worker = match DecoderWorker::new() {
                Ok(worker) => worker,
                Err(error) => {
                    self.error = Some(error);
                    return None;
                }
            };
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
        let Some(worker) = self.workers.get(key) else {
            return;
        };
        worker.drain(&mut output);
        let mut failed = false;
        for item in output {
            match item {
                WorkerOutput::Image { sequence, bytes } => {
                    if bytes.len() > MAX_DECODED_IMAGE_BYTES {
                        self.error =
                            Some("Device video decoder returned an oversized image".into());
                        failed = true;
                        continue;
                    }
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
                        session_epoch: key.3.clone(),
                        sequence,
                        device_id: key.2.clone(),
                        screen_id: key.4,
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
        self.awaiting_keyframe.insert(key.clone());
        self.restart_worker(key);
    }
}

impl Drop for DeviceVideoDecoder {
    fn drop(&mut self) {
        for worker in self.workers.values_mut() {
            worker.shutdown();
        }
    }
}

fn run_decoder_worker(
    command_rx: Receiver<WorkerCommand>,
    output_tx: SyncSender<WorkerOutput>,
    cancelled: Arc<AtomicBool>,
) {
    let executable = host_daemon::ffmpeg::executable();
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
    let (reader_rx, reader_join) = match spawn_jpeg_reader(stdout, Arc::clone(&cancelled)) {
        Ok(value) => value,
        Err(error) => {
            let _ = output_tx.try_send(WorkerOutput::Error(format!(
                "Device video decoder output worker could not start: {error}"
            )));
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
    };
    let stderr_join = match stderr {
        Some(stderr) => match spawn_stderr_drainer(stderr, Arc::clone(&cancelled)) {
            Ok(join) => Some(join),
            Err(error) => {
                let _ = output_tx.try_send(WorkerOutput::Error(format!(
                    "Device video decoder stderr worker could not start: {error}"
                )));
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader_join.join();
                return;
            }
        },
        None => None,
    };
    let (writer_tx, writer_rx) = mpsc::sync_channel(MAX_WORKER_QUEUE);
    let writer_join =
        match spawn_decoder_writer(stdin, writer_rx, Arc::clone(&cancelled), output_tx.clone()) {
            Ok(join) => join,
            Err(error) => {
                let _ = output_tx.try_send(WorkerOutput::Error(format!(
                    "Device video decoder input worker could not start: {error}"
                )));
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader_join.join();
                if let Some(join) = stderr_join {
                    let _ = join.join();
                }
                return;
            }
        };
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
) -> std::io::Result<JoinHandle<()>> {
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
}

fn spawn_jpeg_reader(
    stdout: ChildStdout,
    cancelled: Arc<AtomicBool>,
) -> std::io::Result<(Receiver<ReaderEvent>, JoinHandle<()>)> {
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
                if buffer.len() > MAX_DECODED_IMAGE_BYTES {
                    let _ = events.try_send(ReaderEvent::Error(
                        "Device video decoder returned an oversized image stream".into(),
                    ));
                    return;
                }
                while let Some((start, end)) = jpeg_bounds(&buffer) {
                    let image = buffer[start..end].to_vec();
                    buffer.drain(..end);
                    if image.len() > MAX_DECODED_IMAGE_BYTES {
                        let _ = events.try_send(ReaderEvent::Error(
                            "Device video decoder returned an oversized image".into(),
                        ));
                        return;
                    }
                    if events.try_send(ReaderEvent::Image(image)).is_err() {
                        let _ = events.try_send(ReaderEvent::Error(
                            "Device video decoder output queue is full".into(),
                        ));
                        return;
                    }
                }
            }
        })?;
    Ok((receiver, join))
}

fn spawn_stderr_drainer(
    stderr: std::process::ChildStderr,
    cancelled: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
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
    let mut offset: usize = 0;
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
        let Some(length_bytes) = bytes.get(offset..offset + 2) else {
            return Vec::new();
        };
        let length = u16::from_be_bytes([length_bytes[0], length_bytes[1]]) as usize;
        offset += 2;
        let Some(sps) = bytes.get(offset..offset + length) else {
            return Vec::new();
        };
        result.extend_from_slice(&[0, 0, 0, 1]);
        result.extend_from_slice(sps);
        offset += length;
    }
    let Some(&pps_count) = bytes.get(offset) else {
        return result;
    };
    offset += 1;
    for _ in 0..pps_count as usize {
        let Some(length_bytes) = bytes.get(offset..offset + 2) else {
            return Vec::new();
        };
        let length = u16::from_be_bytes([length_bytes[0], length_bytes[1]]) as usize;
        offset += 2;
        let Some(pps) = bytes.get(offset..offset + length) else {
            return Vec::new();
        };
        result.extend_from_slice(&[0, 0, 0, 1]);
        result.extend_from_slice(pps);
        offset += length;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_admission_rejects_duplicates_and_marks_gaps() {
        assert_eq!(
            sequence_disposition(None, 1),
            SequenceDisposition::FirstOrContiguous
        );
        assert_eq!(
            sequence_disposition(Some(1), 2),
            SequenceDisposition::FirstOrContiguous
        );
        assert_eq!(
            sequence_disposition(Some(1), 1),
            SequenceDisposition::Duplicate
        );
        assert_eq!(sequence_disposition(Some(1), 4), SequenceDisposition::Gap);
    }

    #[test]
    fn avcc_description_is_retained_as_annex_b_configuration() {
        let description = [
            1, 0x64, 0, 0x1f, 0xff, 0xe1, 0, 2, 0x67, 1, 1, 0, 2, 0x68, 2,
        ];
        assert_eq!(
            description_to_annex_b(&description),
            vec![0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2,]
        );
        assert!(description_to_annex_b(&description).starts_with(&[0, 0, 0, 1, 0x67, 1]));
    }

    #[test]
    fn access_unit_bound_is_separate_from_inflight_queue_bound() {
        assert_eq!(MAX_ACCESS_UNIT_BYTES, 8 * 1024 * 1024);
        assert_eq!(MAX_DECODED_IMAGE_BYTES, 16 * 1024 * 1024);
        assert_eq!(MAX_WORKER_QUEUE, 8);
        assert_eq!(MAX_WORKER_OUTPUT, 8);
    }

    #[test]
    fn annex_b_conversion_preserves_complete_access_units() {
        let bytes = [0, 0, 0, 2, 0x65, 1, 0, 0, 0, 2, 0x41, 2];
        assert_eq!(
            to_annex_b(&bytes),
            vec![0, 0, 0, 1, 0x65, 1, 0, 0, 0, 1, 0x41, 2]
        );
    }
}
