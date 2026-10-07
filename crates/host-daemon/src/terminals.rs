//! Thread-owned PTYs, kept across transport disconnects. Any paired device may
//! attach and every attached session receives the output; the Host itself opens
//! terminals for setup scripts. The private supervisor pipe carries terminal I/O;
//! only this owner publishes events. Each terminal's screen is kept in a history
//! file, so a terminal opened again after a Host restart shows it.
use crate::host_rpc::connections::{Connections, SessionId};
use agent_domain::{ThreadId, Timestamp};
use agent_protocol::{
    models::Empty,
    operations::{
        ClearTerminal, RestartTerminal, StartTerminal, TerminalMetadataEvent, TerminalSize,
        TerminalStatus, TerminalSummary, terminal_label, thread_terminal_handle_for,
    },
    protocol::{Call, Notification},
};
use agent_transport::peer::JsonlReader;
use alacritty_terminal::grid::Dimensions as _;
use bex_process::{PtyCommand, PtyEvent};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    sync::{broadcast, mpsc, oneshot, watch},
};
use tokio_util::sync::CancellationToken;

pub(crate) mod processes;
use processes::{ProcessSource, ProcessTable, Subprocess, poll_delay};

const MAX_RETAINED_INACTIVE: usize = 128;
const MAX_LABEL_LENGTH: usize = 128;
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const DEFAULT_SIZE: TerminalSize = TerminalSize {
    cols: 120,
    rows: 30,
};
/// Lines a terminal keeps above its screen.
const SCROLLBACK_LINES: usize = 5_000;
/// Output is written to the history file at most this often.
const PERSIST_DELAY: Duration = Duration::from_millis(40);
/// A kept screen larger than one frame cannot be restored.
const MAX_HISTORY_BYTES: usize = agent_protocol::protocol::MAX_FRAME_BYTES;
/// Host variables a user's shell must not inherit.
const ENV_BLOCKLIST: [&str; 3] = ["PORT", "ELECTRON_RENDERER_PORT", "ELECTRON_RUN_AS_NODE"];
const APP_ENV_PREFIXES: [&str; 2] = ["BEX_", "VITE_"];

#[derive(Clone, Default)]
struct Replies(Arc<Mutex<Vec<alacritty_terminal::event::Event>>>);
impl alacritty_terminal::event::EventListener for Replies {
    fn send_event(&self, event: alacritty_terminal::event::Event) {
        use alacritty_terminal::event::Event;
        if matches!(
            event,
            Event::PtyWrite(_)
                | Event::ColorRequest(..)
                | Event::TextAreaSizeRequest(_)
                | Event::ClipboardLoad(..)
        ) {
            self.0.lock().unwrap().push(event);
        }
    }
}
struct Dimensions(TerminalSize);
impl alacritty_terminal::grid::Dimensions for Dimensions {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }
    fn screen_lines(&self) -> usize {
        self.0.rows as usize
    }
    fn columns(&self) -> usize {
        self.0.cols as usize
    }
}

fn new_screen(size: TerminalSize, replies: &Replies) -> alacritty_terminal::Term<Replies> {
    let config = alacritty_terminal::term::Config {
        scrolling_history: SCROLLBACK_LINES,
        ..Default::default()
    };
    alacritty_terminal::Term::new(config, &Dimensions(size), replies.clone())
}

/// What an empty terminal of this size shows.
fn blank_screen(size: TerminalSize) -> Vec<u8> {
    new_screen(size, &Replies::default()).ansi_checkpoint(None)
}

/// The history files, one per thread terminal, named from both IDs.
#[derive(Clone)]
struct HistoryFiles(PathBuf);
impl HistoryFiles {
    fn encoded(text: &str) -> String {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text)
    }
    fn path(&self, thread: &ThreadId, terminal_id: &str) -> PathBuf {
        self.0.join(format!(
            "{}.{}.log",
            Self::encoded(thread.as_str()),
            Self::encoded(terminal_id)
        ))
    }
    /// The kept screen; nothing when there is none or it cannot be restored.
    async fn read(path: PathBuf) -> Vec<u8> {
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            let file = match std::fs::File::open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return vec![],
                Err(error) => {
                    tracing::warn!(operation = "host.terminal.history", message = %error);
                    return vec![];
                }
            };
            let mut data = vec![];
            if let Err(error) = file
                .take(MAX_HISTORY_BYTES as u64 + 1)
                .read_to_end(&mut data)
            {
                tracing::warn!(operation = "host.terminal.history", message = %error);
                return vec![];
            }
            if data.len() > MAX_HISTORY_BYTES {
                return vec![];
            }
            data
        })
        .await
        .unwrap_or_default()
    }
    async fn delete(path: PathBuf) {
        let _ = tokio::task::spawn_blocking(move || {
            if let Err(error) = std::fs::remove_file(&path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(operation = "host.terminal.history", message = %error);
            }
        })
        .await;
    }
    /// Deletes the history of every terminal the thread had.
    async fn delete_thread(&self, thread: &ThreadId) {
        let (directory, prefix) = (
            self.0.clone(),
            format!("{}.", Self::encoded(thread.as_str())),
        );
        let _ = tokio::task::spawn_blocking(move || {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                return;
            };
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with(&prefix)
                    && let Err(error) = std::fs::remove_file(entry.path())
                {
                    tracing::warn!(operation = "host.terminal.history", message = %error);
                }
            }
        })
        .await;
    }
}

/// Writes one terminal's screen to its history file, a write at a time.
struct Saver {
    path: PathBuf,
    writing: Option<tokio::task::JoinHandle<()>>,
}
impl Saver {
    async fn save(&mut self, data: Vec<u8>) {
        if let Some(writing) = self.writing.take() {
            let _ = writing.await;
        }
        let path = self.path.clone();
        self.writing = Some(tokio::task::spawn_blocking(move || {
            if let Err(error) = crate::platform::save_private_bytes(&path, &data) {
                tracing::warn!(operation = "host.terminal.history", message = %error);
            }
        }));
    }
    /// Writes `data` and waits until it is on disk.
    async fn flush(&mut self, data: Vec<u8>) {
        self.save(data).await;
        if let Some(writing) = self.writing.take() {
            let _ = writing.await;
        }
    }
}

/// A terminal the Host opens for itself, such as a setup script's.
pub(crate) struct OpenTerminal {
    pub(crate) thread: ThreadId,
    pub(crate) terminal_id: String,
    pub(crate) cwd: String,
    pub(crate) worktree_path: Option<String>,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) size: Option<TerminalSize>,
}

/// What a Host-side observer of one terminal receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TerminalOutput {
    Data(Vec<u8>),
    Exited,
    Closed,
}

type Receipt = oneshot::Sender<Result<(), String>>;
struct Command {
    action: Action,
    complete: Receipt,
}
enum Action {
    Write(Vec<u8>),
    Resize(TerminalSize),
    Attach(SessionId, TerminalSize),
    Clear,
}

/// Delivery shared by every process a terminal runs over its lifetime.
struct Channel {
    handle: String,
    router: Connections,
    attached: Mutex<BTreeSet<SessionId>>,
    listeners: Mutex<Vec<mpsc::UnboundedSender<TerminalOutput>>>,
    /// Output chunks and input writes; only grows.
    activity: AtomicU64,
}
impl Channel {
    fn publish(&self, event: &Notification) {
        let sessions: Vec<_> = self.attached.lock().unwrap().iter().copied().collect();
        for session in sessions {
            if self.router.send(session, event.clone()).is_err() {
                self.attached.lock().unwrap().remove(&session);
            }
        }
    }
    fn observe(&self, output: TerminalOutput) {
        self.listeners
            .lock()
            .unwrap()
            .retain(|listener| listener.send(output.clone()).is_ok());
    }
    fn send(&self, session: SessionId, event: Notification) -> Result<(), String> {
        let result = self.router.send(session, event);
        if result.is_err() {
            self.attached.lock().unwrap().remove(&session);
        }
        result
    }
}

struct Process {
    generation: u64,
    input: mpsc::Sender<Command>,
    stop: CancellationToken,
    finished: watch::Receiver<Option<Result<(), String>>>,
}

struct Record {
    thread: ThreadId,
    terminal_id: String,
    cwd: PathBuf,
    worktree_path: Option<String>,
    env: BTreeMap<String, String>,
    size: TerminalSize,
    status: TerminalStatus,
    pid: Option<u32>,
    exit_code: Option<i32>,
    subprocess: Subprocess,
    updated_at: Timestamp,
    channel: Arc<Channel>,
    process: Option<Process>,
    /// The last screen of a terminal that is not running.
    screen: Vec<u8>,
    failure: Option<String>,
    history: PathBuf,
}
impl Record {
    fn summary(&self) -> TerminalSummary {
        let label = match &self.subprocess {
            Subprocess::Running(Some(label)) if !label.trim().is_empty() => label.trim().to_owned(),
            _ => terminal_label(&self.terminal_id),
        };
        TerminalSummary {
            thread: self.thread.clone(),
            terminal_id: self.terminal_id.clone(),
            cwd: self.cwd.to_string_lossy().into_owned(),
            worktree_path: self.worktree_path.clone(),
            status: self.status,
            pid: self.pid,
            exit_code: self.exit_code,
            has_running_subprocess: self.subprocess.running(),
            label: label.chars().take(MAX_LABEL_LENGTH).collect(),
            updated_at: self.updated_at.clone(),
        }
    }
    fn running(&self) -> bool {
        matches!(
            self.status,
            TerminalStatus::Starting | TerminalStatus::Running
        )
    }
    fn touch(&mut self) {
        self.updated_at = now();
    }
    /// Stops the shell and forgets the launch context and screen; the caller
    /// waits for the returned process to finish.
    fn reset(
        &mut self,
        cwd: PathBuf,
        worktree_path: Option<String>,
        env: BTreeMap<String, String>,
    ) -> Option<watch::Receiver<Option<Result<(), String>>>> {
        let stopping = self.process.take().map(|process| {
            process.stop.cancel();
            process.finished
        });
        self.cwd = cwd;
        self.worktree_path = worktree_path;
        self.env = env;
        self.screen.clear();
        stopping
    }
}

fn now() -> Timestamp {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64);
    Timestamp::from_millis(millis).expect("the clock is within the timestamp range")
}

struct Inner {
    router: Connections,
    records: Mutex<HashMap<String, Record>>,
    /// Serializes opening, attaching and closing per thread.
    locks: Mutex<HashMap<ThreadId, Arc<tokio::sync::Mutex<()>>>>,
    metadata: broadcast::Sender<TerminalMetadataEvent>,
    processes: ProcessSource,
    poll_interval: Duration,
    poller: OnceLock<tokio_util::task::AbortOnDropHandle<()>>,
    generation: AtomicU64,
    history: HistoryFiles,
}
impl Inner {
    fn upsert(&self, record: &Record) {
        let _ = self.metadata.send(TerminalMetadataEvent::Upsert {
            terminal: record.summary(),
        });
    }
    fn lock(&self, thread: &ThreadId) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .lock()
            .unwrap()
            .entry(thread.clone())
            .or_default()
            .clone()
    }
    /// The shell is up: the terminal runs.
    fn started(&self, handle: &str, generation: u64, pid: Option<u32>) {
        let mut records = self.records.lock().unwrap();
        if let Some(record) = records.get_mut(handle).filter(|record| {
            record
                .process
                .as_ref()
                .is_some_and(|process| process.generation == generation)
        }) {
            record.status = TerminalStatus::Running;
            record.pid = pid;
            record.touch();
            self.upsert(record);
        }
    }
    /// The process is gone; the record stays with its last screen until closed.
    fn finished(&self, handle: &str, generation: u64, outcome: Outcome, screen: Vec<u8>) {
        let mut records = self.records.lock().unwrap();
        let Some(record) = records.get_mut(handle).filter(|record| {
            record
                .process
                .as_ref()
                .is_some_and(|process| process.generation == generation)
        }) else {
            return;
        };
        record.process = None;
        record.pid = None;
        record.subprocess = Subprocess::Idle;
        record.screen = screen;
        match outcome {
            Outcome::Exited(code) => {
                record.status = TerminalStatus::Exited;
                record.exit_code = code;
                record.failure = None;
            }
            Outcome::Failed(reason) => {
                record.status = TerminalStatus::Error;
                record.exit_code = None;
                record.failure = Some(reason);
            }
        }
        record.touch();
        self.upsert(record);
        Self::evict(&mut records);
    }
    /// Keeps the newest inactive terminals; older ones go without an event.
    /// Their history stays.
    fn evict(records: &mut HashMap<String, Record>) {
        let mut inactive: Vec<(Timestamp, String)> = records
            .iter()
            .filter(|(_, record)| !record.running())
            .map(|(handle, record)| (record.updated_at.clone(), handle.clone()))
            .collect();
        if inactive.len() <= MAX_RETAINED_INACTIVE {
            return;
        }
        inactive.sort();
        for (_, handle) in inactive
            .into_iter()
            .take(records.len().saturating_sub(MAX_RETAINED_INACTIVE))
        {
            records.remove(&handle);
        }
    }
    /// Applies one process snapshot to every running terminal.
    fn apply(&self, table: &ProcessTable) {
        let mut records = self.records.lock().unwrap();
        for record in records.values_mut() {
            let (TerminalStatus::Running, Some(pid)) = (record.status, record.pid) else {
                continue;
            };
            let next = table.subprocess(pid);
            if next != record.subprocess {
                record.subprocess = next;
                record.channel.activity.fetch_add(1, Ordering::AcqRel);
                record.touch();
                self.upsert(record);
            }
        }
    }
    fn running_shells(&self) -> bool {
        self.records
            .lock()
            .unwrap()
            .values()
            .any(|record| record.status == TerminalStatus::Running && record.pid.is_some())
    }
}

enum Outcome {
    Exited(Option<i32>),
    Failed(String),
}

pub(crate) struct Terminals {
    inner: Arc<Inner>,
}
impl Drop for Terminals {
    fn drop(&mut self) {
        for record in self.inner.records.lock().unwrap().values() {
            if let Some(process) = &record.process {
                process.stop.cancel();
            }
        }
    }
}

/// The shell's environment: the Host's without its own variables, then the
/// caller's, with `~` expanded in provider homes.
fn shell_environment(overlay: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let home = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_owned());
    environment(std::env::vars(), overlay, home.as_deref())
}

fn environment(
    base: impl IntoIterator<Item = (String, String)>,
    overlay: &BTreeMap<String, String>,
    home: Option<&Path>,
) -> BTreeMap<String, String> {
    let blocked = |key: &str| {
        let upper = key.to_ascii_uppercase();
        ENV_BLOCKLIST.contains(&upper.as_str())
            || APP_ENV_PREFIXES
                .iter()
                .any(|prefix| upper.starts_with(prefix))
    };
    let mut env: BTreeMap<String, String> =
        base.into_iter().filter(|(key, _)| !blocked(key)).collect();
    for (key, value) in overlay {
        let home_relative = value
            .strip_prefix("~/")
            .or_else(|| value.strip_prefix("~\\"));
        let value = match (key.as_str(), home, home_relative) {
            ("CODEX_HOME" | "CLAUDE_CONFIG_DIR", Some(home), _) if value == "~" => {
                home.to_string_lossy().into_owned()
            }
            ("CODEX_HOME" | "CLAUDE_CONFIG_DIR", Some(home), Some(rest)) => {
                home.join(rest).to_string_lossy().into_owned()
            }
            _ => value.clone(),
        };
        env.insert(key.clone(), value);
    }
    env.entry("COLORTERM".into())
        .or_insert_with(|| "truecolor".into());
    env
}

/// The terminal's working directory, which must exist as a directory.
async fn directory(cwd: &str) -> Result<PathBuf, String> {
    let metadata = match tokio::fs::metadata(cwd).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("Terminal cwd does not exist: {cwd}"));
        }
        Err(_) => return Err(format!("Failed to access terminal cwd: {cwd}")),
    };
    if !metadata.is_dir() {
        return Err(format!("Terminal cwd is not a directory: {cwd}"));
    }
    let resolved = tokio::fs::canonicalize(cwd)
        .await
        .map_err(|_| format!("Failed to access terminal cwd: {cwd}"))?;
    Ok(dunce::simplified(&resolved).to_owned())
}

fn unknown(thread: &ThreadId, terminal_id: &str) -> String {
    format!("Unknown terminal thread: {thread}, terminal: {terminal_id}")
}

impl Terminals {
    pub(crate) fn new(router: Connections, history: PathBuf) -> Self {
        Self::with_processes(router, processes::system(), POLL_INTERVAL, history)
    }

    pub(crate) fn with_processes(
        router: Connections,
        processes: ProcessSource,
        interval: Duration,
        history: PathBuf,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                router,
                records: Mutex::default(),
                locks: Mutex::default(),
                metadata: broadcast::channel(256).0,
                processes,
                poll_interval: interval,
                poller: OnceLock::new(),
                generation: AtomicU64::new(0),
                history: HistoryFiles(history),
            }),
        }
    }

    /// Every terminal now, and the changes after it. A subscriber that falls
    /// behind takes a new snapshot.
    pub(crate) fn subscribe_metadata(
        &self,
    ) -> (
        Vec<TerminalSummary>,
        broadcast::Receiver<TerminalMetadataEvent>,
    ) {
        let records = self.inner.records.lock().unwrap();
        let receiver = self.inner.metadata.subscribe();
        (Self::summaries(&records), receiver)
    }

    pub(crate) fn summaries_now(&self) -> Vec<TerminalSummary> {
        Self::summaries(&self.inner.records.lock().unwrap())
    }

    fn summaries(records: &HashMap<String, Record>) -> Vec<TerminalSummary> {
        let mut terminals: Vec<_> = records.values().map(Record::summary).collect();
        terminals.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.thread.cmp(&b.thread))
                .then_with(|| a.terminal_id.cmp(&b.terminal_id))
        });
        terminals
    }

    /// Output of one terminal from now on, until it closes.
    pub(crate) fn observe(
        &self,
        thread: &ThreadId,
        terminal_id: &str,
    ) -> Option<mpsc::UnboundedReceiver<TerminalOutput>> {
        let handle = thread_terminal_handle_for(thread.as_str(), terminal_id);
        let records = self.inner.records.lock().unwrap();
        let record = records.get(&handle)?;
        let (sender, receiver) = mpsc::unbounded_channel();
        record.channel.listeners.lock().unwrap().push(sender);
        Some(receiver)
    }

    fn ensure_poller(&self) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        self.inner.poller.get_or_init(|| {
            tokio_util::task::AbortOnDropHandle::new(tokio::spawn(poll(Arc::downgrade(
                &self.inner,
            ))))
        });
    }

    /// A new terminal for `request`, which runs in `cwd` at `size`.
    fn record(&self, request: OpenTerminal, cwd: PathBuf, size: TerminalSize) -> Record {
        let handle = thread_terminal_handle_for(request.thread.as_str(), &request.terminal_id);
        Record {
            history: self
                .inner
                .history
                .path(&request.thread, &request.terminal_id),
            thread: request.thread,
            terminal_id: request.terminal_id,
            cwd,
            worktree_path: request.worktree_path,
            env: request.env,
            size,
            status: TerminalStatus::Starting,
            pid: None,
            exit_code: None,
            subprocess: Subprocess::Idle,
            updated_at: now(),
            channel: Arc::new(Channel {
                handle,
                router: self.inner.router.clone(),
                attached: Mutex::default(),
                listeners: Mutex::default(),
                activity: AtomicU64::new(0),
            }),
            process: None,
            screen: vec![],
            failure: None,
        }
    }

    /// Opens the terminal: starts it, restarts it when it exited or its
    /// directory or environment changed, or resizes the running shell.
    pub(crate) async fn open(&self, request: OpenTerminal) -> Result<(), String> {
        agent_protocol::operations::validate_terminal_id(&request.terminal_id)?;
        agent_protocol::operations::validate_terminal_env(&request.env)?;
        let size = request.size.unwrap_or(DEFAULT_SIZE);
        size.validate()?;
        let cwd = directory(request.cwd.trim()).await?;
        let lock = self.inner.lock(&request.thread);
        let _guard = lock.lock().await;
        self.open_locked(request, cwd, size).await
    }

    /// A terminal new to this Host starts on its kept screen; one that starts
    /// again starts empty.
    async fn open_locked(
        &self,
        request: OpenTerminal,
        cwd: PathBuf,
        size: TerminalSize,
    ) -> Result<(), String> {
        let handle = thread_terminal_handle_for(request.thread.as_str(), &request.terminal_id);
        let (stopping, kept, history) = {
            let mut records = self.inner.records.lock().unwrap();
            match records.get_mut(&handle) {
                None => {
                    let record = self.record(
                        OpenTerminal {
                            thread: request.thread.clone(),
                            terminal_id: request.terminal_id.clone(),
                            cwd: request.cwd.clone(),
                            worktree_path: request.worktree_path.clone(),
                            env: request.env.clone(),
                            size: Some(size),
                        },
                        cwd.clone(),
                        size,
                    );
                    let history = record.history.clone();
                    records.insert(handle.clone(), record);
                    (None, true, history)
                }
                Some(record) => {
                    let changed = record.cwd != cwd
                        || record.env != request.env
                        || record.worktree_path != request.worktree_path;
                    let stopping = (changed || record.process.is_none())
                        .then(|| {
                            record.reset(
                                cwd.clone(),
                                request.worktree_path.clone(),
                                request.env.clone(),
                            )
                        })
                        .flatten();
                    (stopping, false, record.history.clone())
                }
            }
        };
        if let Some(finished) = stopping {
            wait(finished).await?;
        }
        let (running, resize) = {
            let records = self.inner.records.lock().unwrap();
            let record = records.get(&handle).ok_or("terminal was closed")?;
            (
                record.process.is_some(),
                record
                    .process
                    .as_ref()
                    .filter(|_| record.size != size)
                    .map(|process| process.input.clone()),
            )
        };
        if !running {
            let initial = if kept {
                HistoryFiles::read(history).await
            } else {
                HistoryFiles::delete(history).await;
                vec![]
            };
            return self.start(&handle, size, initial).await;
        }
        if let Some(input) = resize {
            send(&input, Action::Resize(size)).await?;
            if let Some(record) = self.inner.records.lock().unwrap().get_mut(&handle) {
                record.size = size;
                record.touch();
            }
        }
        Ok(())
    }

    /// Runs a new shell for the record under `handle`, its screen starting as
    /// `initial`, and waits until it is up.
    async fn start(
        &self,
        handle: &str,
        size: TerminalSize,
        initial: Vec<u8>,
    ) -> Result<(), String> {
        self.ensure_poller();
        let generation = self.inner.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let (input, receiver) = mpsc::channel(32);
        let (complete, finished) = watch::channel(None);
        let stop = CancellationToken::new();
        let (channel, cwd, env, history) = {
            let mut records = self.inner.records.lock().unwrap();
            let record = records.get_mut(handle).ok_or("terminal was closed")?;
            record.process = Some(Process {
                generation,
                input,
                stop: stop.clone(),
                finished,
            });
            record.status = TerminalStatus::Starting;
            record.pid = None;
            record.exit_code = None;
            record.failure = None;
            record.subprocess = Subprocess::Idle;
            record.size = size;
            record.touch();
            self.inner.upsert(record);
            (
                record.channel.clone(),
                record.cwd.clone(),
                shell_environment(&record.env),
                record.history.clone(),
            )
        };
        let (ready, started) = oneshot::channel();
        let inner = Arc::downgrade(&self.inner);
        let handle = handle.to_owned();
        tokio::spawn(async move {
            let worker = Worker {
                channel,
                inner: inner.clone(),
                generation,
                stop,
                input: receiver,
                ready: Some(ready),
                saver: Saver {
                    path: history,
                    writing: None,
                },
            };
            let (cleanup, outcome, screen) = worker.run(cwd, size, env, initial).await;
            if let Some(inner) = inner.upgrade() {
                inner.finished(&handle, generation, outcome, screen);
            }
            complete.send_replace(Some(cleanup));
        });
        tokio::time::timeout(Duration::from_secs(10), started)
            .await
            .map_err(|_| "terminal startup timed out")?
            .map_err(|_| "terminal startup stopped")?
    }

    /// `host/terminal/start`: attaches `session` to the thread's terminal,
    /// opening it first when it does not exist, or restarting an exited one on
    /// request.
    pub(crate) async fn attach(
        &self,
        session: SessionId,
        params: &StartTerminal,
    ) -> Result<Empty, String> {
        params.validate()?;
        self.inner.router.ensure_session(session)?;
        let handle = params.handle();
        let cwd = match &params.cwd {
            Some(cwd) => Some(directory(cwd.trim()).await?),
            None => None,
        };
        let lock = self.inner.lock(&params.thread);
        let _guard = lock.lock().await;
        let running = {
            let records = self.inner.records.lock().unwrap();
            records.get(&handle).map(|record| record.process.is_some())
        };
        let open = match running {
            None => true,
            Some(false) => params.restart_if_not_running && cwd.is_some(),
            Some(true) => false,
        };
        if open {
            let Some(cwd) = cwd else {
                return Err(unknown(&params.thread, &params.terminal_id));
            };
            self.open_locked(
                OpenTerminal {
                    thread: params.thread.clone(),
                    terminal_id: params.terminal_id.clone(),
                    cwd: cwd.to_string_lossy().into_owned(),
                    worktree_path: params.worktree_path.clone(),
                    env: params.env.clone(),
                    size: Some(params.size),
                },
                cwd,
                params.size,
            )
            .await?;
        }
        let (channel, input, restored) = {
            let records = self.inner.records.lock().unwrap();
            let record = records.get(&handle).ok_or("terminal was closed")?;
            record.channel.attached.lock().unwrap().insert(session);
            let restored = (record.process.is_none()).then(|| {
                let ended = match (&record.status, &record.failure) {
                    (TerminalStatus::Error, Some(reason)) => Notification::TerminalFailed {
                        handle: handle.clone(),
                        reason: reason.clone(),
                    },
                    _ => Notification::Exited {
                        handle: handle.clone(),
                        code: record.exit_code.unwrap_or(0),
                    },
                };
                (record.screen.clone(), record.size, ended)
            });
            (
                record.channel.clone(),
                record.process.as_ref().map(|process| process.input.clone()),
                restored,
            )
        };
        match (input, restored) {
            (Some(input), _) => send(&input, Action::Attach(session, params.size)).await?,
            (None, Some((data, size, ended))) => {
                channel.send(
                    session,
                    Notification::TerminalRestored {
                        handle: handle.clone(),
                        data,
                        cols: size.cols,
                        rows: size.rows,
                    },
                )?;
                channel.send(session, ended)?;
            }
            (None, None) => return Err("terminal was closed".into()),
        }
        Ok(Empty {})
    }

    /// `host/terminal/clear`: empties the terminal's history and every
    /// attached screen; a running shell keeps running.
    pub(crate) async fn clear(&self, params: &ClearTerminal) -> Result<Empty, String> {
        agent_protocol::operations::validate_terminal_id(&params.terminal_id)?;
        let lock = self.inner.lock(&params.thread);
        let _guard = lock.lock().await;
        let handle = params.handle();
        let (input, cleared) = {
            let mut records = self.inner.records.lock().unwrap();
            let record = records
                .get_mut(&handle)
                .ok_or_else(|| unknown(&params.thread, &params.terminal_id))?;
            match &record.process {
                Some(process) => (Some(process.input.clone()), None),
                None => {
                    record.screen = blank_screen(record.size);
                    record.touch();
                    (
                        None,
                        Some((
                            record.channel.clone(),
                            record.screen.clone(),
                            record.size,
                            record.history.clone(),
                        )),
                    )
                }
            }
        };
        match (input, cleared) {
            (Some(input), _) => send(&input, Action::Clear).await?,
            (None, Some((channel, data, size, history))) => {
                HistoryFiles::delete(history).await;
                channel.publish(&Notification::TerminalRestored {
                    handle,
                    data,
                    cols: size.cols,
                    rows: size.rows,
                });
            }
            (None, None) => {}
        }
        Ok(Empty {})
    }

    /// `host/terminal/restart`: a new shell in `cwd` with an empty history,
    /// after stopping the running one; opens the terminal when it does not
    /// exist.
    pub(crate) async fn restart(&self, params: &RestartTerminal) -> Result<Empty, String> {
        params.validate()?;
        let cwd = directory(params.cwd.trim()).await?;
        let lock = self.inner.lock(&params.thread);
        let _guard = lock.lock().await;
        let handle = params.handle();
        let (stopping, history) = {
            let mut records = self.inner.records.lock().unwrap();
            match records.get_mut(&handle) {
                Some(record) => (
                    record.reset(cwd, params.worktree_path.clone(), params.env.clone()),
                    record.history.clone(),
                ),
                None => {
                    let record = self.record(
                        OpenTerminal {
                            thread: params.thread.clone(),
                            terminal_id: params.terminal_id.clone(),
                            cwd: params.cwd.clone(),
                            worktree_path: params.worktree_path.clone(),
                            env: params.env.clone(),
                            size: Some(params.size),
                        },
                        cwd,
                        params.size,
                    );
                    let history = record.history.clone();
                    records.insert(handle.clone(), record);
                    (None, history)
                }
            }
        };
        if let Some(finished) = stopping {
            wait(finished).await?;
        }
        HistoryFiles::delete(history).await;
        self.start(&handle, params.size, vec![]).await?;
        Ok(Empty {})
    }

    /// Input for a running terminal; an exited one ignores it.
    pub(crate) async fn write(&self, handle: &str, data: Vec<u8>) -> Result<(), String> {
        if data.is_empty() || data.len() > 64 * 1024 {
            return Err("terminal input must be 1 byte to 64 KiB".into());
        }
        let input = {
            let records = self.inner.records.lock().unwrap();
            let record = records
                .get(handle)
                .ok_or_else(|| format!("Unknown terminal: {handle}"))?;
            match (record.status, &record.process) {
                (TerminalStatus::Exited, _) => return Ok(()),
                (TerminalStatus::Running, Some(process)) => {
                    record.channel.activity.fetch_add(1, Ordering::AcqRel);
                    process.input.clone()
                }
                _ => return Err(format!("Terminal is not running: {handle}")),
            }
        };
        send(&input, Action::Write(data)).await
    }

    /// The RPC operations on an existing terminal.
    pub(crate) async fn request(&self, session: SessionId, call: &Call) -> Result<Empty, String> {
        match call {
            Call::WriteTerminal(params) => {
                self.write(&params.process_handle, params.data.clone())
                    .await?
            }
            Call::ResizeTerminal(params) => {
                params.size.validate()?;
                let input = {
                    let mut records = self.inner.records.lock().unwrap();
                    let Some(record) = records.get_mut(&params.handle) else {
                        return Ok(Empty {});
                    };
                    let Some(process) = record
                        .process
                        .as_ref()
                        .filter(|_| record.status == TerminalStatus::Running)
                    else {
                        return Ok(Empty {});
                    };
                    let input = process.input.clone();
                    record.size = params.size;
                    record.touch();
                    input
                };
                send(&input, Action::Resize(params.size)).await?;
            }
            Call::DetachTerminal(params) => {
                if let Some(record) = self.inner.records.lock().unwrap().get(&params.handle) {
                    record.channel.attached.lock().unwrap().remove(&session);
                }
            }
            Call::KillTerminal(params) => {
                self.close(&params.process_handle, params.delete_history)
                    .await?
            }
            Call::ClearTerminal(params) => return self.clear(params).await,
            Call::RestartTerminal(params) => return self.restart(params).await,
            _ => return Err("unknown terminal operation".into()),
        }
        Ok(Empty {})
    }

    /// Stops the terminal's shell and forgets it; its history stays unless
    /// `delete_history`.
    pub(crate) async fn close(&self, handle: &str, delete_history: bool) -> Result<(), String> {
        let Some(record) = self.inner.records.lock().unwrap().remove(handle) else {
            return Ok(());
        };
        let _ = self.inner.metadata.send(TerminalMetadataEvent::Remove {
            thread: record.thread.clone(),
            terminal_id: record.terminal_id.clone(),
        });
        record.channel.publish(&Notification::TerminalClosed {
            handle: handle.to_owned(),
        });
        record.channel.observe(TerminalOutput::Closed);
        let stopped = match record.process {
            Some(process) => {
                process.stop.cancel();
                wait(process.finished).await
            }
            None => Ok(()),
        };
        if delete_history {
            HistoryFiles::delete(record.history).await;
        }
        stopped
    }

    /// Closes every terminal of the thread and deletes their history.
    pub(crate) async fn close_thread(&self, thread: &ThreadId) {
        let lock = self.inner.lock(thread);
        let _guard = lock.lock().await;
        let handles: Vec<String> = self
            .inner
            .records
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, record)| &record.thread == thread)
            .map(|(handle, _)| handle.clone())
            .collect();
        for handle in handles {
            if let Err(error) = self.close(&handle, false).await {
                tracing::warn!(operation = "host.terminal.cleanup", message = %error);
            }
        }
        self.inner.history.delete_thread(thread).await;
        self.inner.locks.lock().unwrap().remove(thread);
    }

    /// Closes the thread's running shells that run nothing and saw no input or
    /// output during the check, keeping their history. A failed check closes
    /// nothing.
    pub(crate) async fn close_idle(&self, thread: &ThreadId, terminal_id: Option<&str>) {
        let lock = self.inner.lock(thread);
        let _guard = lock.lock().await;
        let marks: Vec<(String, u32, u64)> = self
            .inner
            .records
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, record)| {
                &record.thread == thread
                    && record.status == TerminalStatus::Running
                    && terminal_id.is_none_or(|id| record.terminal_id == id)
            })
            .filter_map(|(handle, record)| {
                Some((
                    handle.clone(),
                    record.pid?,
                    record.channel.activity.load(Ordering::Acquire),
                ))
            })
            .collect();
        if marks.is_empty() {
            return;
        }
        let table = match (self.inner.processes)().await {
            Ok(table) => table,
            Err(error) => {
                tracing::warn!(operation = "host.terminal.close_idle", message = %error);
                return;
            }
        };
        for (handle, pid, mark) in marks {
            let unchanged = self
                .inner
                .records
                .lock()
                .unwrap()
                .get(&handle)
                .is_some_and(|record| record.channel.activity.load(Ordering::Acquire) == mark);
            if unchanged
                && !table.subprocess(pid).running()
                && let Err(error) = self.close(&handle, false).await
            {
                tracing::warn!(operation = "host.terminal.close_idle", message = %error);
            }
        }
    }

    /// The session's connection closed: it no longer receives output.
    pub(crate) fn close_session(&self, session: SessionId) {
        for record in self.inner.records.lock().unwrap().values() {
            record.channel.attached.lock().unwrap().remove(&session);
        }
    }

    /// A revoked device stops receiving output; its terminals keep running.
    pub(crate) fn revoke_device(&self, principal: &str) {
        for record in self.inner.records.lock().unwrap().values() {
            let revoked: Vec<SessionId> = record
                .channel
                .attached
                .lock()
                .unwrap()
                .iter()
                .copied()
                .filter(|session| {
                    self.inner.router.principal(*session).ok().as_deref() == Some(principal)
                })
                .collect();
            for session in revoked {
                record.channel.attached.lock().unwrap().remove(&session);
                let _ = self.inner.router.send(
                    session,
                    Notification::TerminalDetached {
                        handle: record.channel.handle.clone(),
                    },
                );
            }
        }
    }

    pub(crate) fn in_use(&self, path: &Path) -> bool {
        self.inner
            .records
            .lock()
            .unwrap()
            .values()
            .any(|record| record.running() && record.cwd.starts_with(path))
    }

    /// Stops every shell; each writes its screen to its history first.
    pub(crate) async fn shutdown(&self) {
        let processes: Vec<_> = self
            .inner
            .records
            .lock()
            .unwrap()
            .values()
            .filter_map(|record| {
                let process = record.process.as_ref()?;
                process.stop.cancel();
                Some(process.finished.clone())
            })
            .collect();
        for finished in processes {
            let _ = wait(finished).await;
        }
    }
}

async fn wait(mut finished: watch::Receiver<Option<Result<(), String>>>) -> Result<(), String> {
    loop {
        if let Some(result) = finished.borrow_and_update().clone() {
            return result;
        }
        if finished.changed().await.is_err() {
            return Err("terminal cleanup stopped".into());
        }
    }
}

async fn send(input: &mpsc::Sender<Command>, action: Action) -> Result<(), String> {
    let (complete, completed) = oneshot::channel();
    input
        .send(Command { action, complete })
        .await
        .map_err(|_| "terminal has exited")?;
    completed.await.map_err(|_| "terminal has exited")?
}

/// Inspects running shells for subprocesses, once per interval while any
/// shell runs; failed snapshots back off.
async fn poll(inner: Weak<Inner>) {
    let mut failures = 0u32;
    loop {
        let Some(this) = inner.upgrade() else {
            return;
        };
        let interval = this.poll_interval;
        let delay = if this.running_shells() {
            let source = this.processes.clone();
            drop(this);
            match source().await {
                Ok(table) => {
                    failures = 0;
                    if let Some(this) = inner.upgrade() {
                        this.apply(&table);
                    }
                }
                Err(error) => {
                    failures = (failures + 1).min(30);
                    tracing::warn!(operation = "host.terminal.processes", message = %error);
                }
            }
            poll_delay(interval, failures)
        } else {
            failures = 0;
            drop(this);
            interval
        };
        tokio::time::sleep(delay).await;
    }
}

struct Worker {
    channel: Arc<Channel>,
    inner: Weak<Inner>,
    generation: u64,
    stop: CancellationToken,
    input: mpsc::Receiver<Command>,
    ready: Option<Receipt>,
    saver: Saver,
}
impl Worker {
    fn publish(&self, event: Notification) {
        self.channel.publish(&event);
    }
    async fn run(
        mut self,
        cwd: PathBuf,
        size: TerminalSize,
        env: BTreeMap<String, String>,
        initial: Vec<u8>,
    ) -> (Result<(), String>, Outcome, Vec<u8>) {
        let handle = self.channel.handle.clone();
        let mut pending: Option<(u64, Receipt)> = None;
        let replies = Replies::default();
        let mut screen = new_screen(size, &replies);
        let mut parser: alacritty_terminal::vte::ansi::Processor = Default::default();
        // A kept screen restores state only; it asks the shell nothing.
        parser.advance(&mut screen, &initial);
        replies.0.lock().unwrap().clear();
        let mut persist_at: Option<tokio::time::Instant> = None;
        let mut cleanup = Ok(());
        let mut exit_code = None;
        let result = async {
            if self.stop.is_cancelled() { return Err("terminal startup cancelled".into()); }
            let mut child = bex_process::terminal_command().and_then(|mut command| command.spawn()).map_err(|error| error.to_string())?;
            cleanup = Err("terminal cleanup incomplete".into());
            let mut stdin = child.stdin().take().ok_or("terminal input pipe unavailable")?;
            let mut output = JsonlReader::new(child.stdout().take().ok_or("terminal output pipe unavailable")?);
            let initialize = PtyCommand::Start { command:crate::platform::terminal_command().iter().map(|value| (*value).into()).collect(), cwd:cwd.to_string_lossy().into_owned(), rows:size.rows, cols:size.cols, env };
            let mut next_id = 0u64;
            let mut query_in_flight = false;
            let mut query_bytes = Vec::new();
            let interaction: Result<(), String> = async {
                write(&mut stdin, &initialize).await?;
                loop {
                    tokio::select! {
                        biased;
                        _ = self.stop.cancelled() => return Ok(()),
                        _ = std::future::ready(()), if !query_in_flight && !query_bytes.is_empty() => {
                            query_in_flight = true;
                            write(&mut stdin, &PtyCommand::Write {id:bex_process::TERMINAL_QUERY_REPLY_ID,data:std::mem::take(&mut query_bytes)}).await?;
                        }
                        _ = tokio::time::sleep_until(persist_at.unwrap_or_else(tokio::time::Instant::now)), if persist_at.is_some() => {
                            persist_at = None;
                            self.saver.save(screen.ansi_checkpoint(None)).await;
                        }
                        line = output.read_line() => {
                            let line = line.map_err(|error| error.to_string())?.ok_or("terminal supervisor exited without a result")?;
                            match serde_json::from_str::<PtyEvent>(&line).map_err(|error| error.to_string())? {
                                PtyEvent::Started { pid } => {
                                    if let Some(inner) = self.inner.upgrade() { inner.started(&handle, self.generation, pid); }
                                    self.publish(Notification::TerminalRestored { handle: handle.clone(), data: screen.ansi_checkpoint(None), cols: size.cols, rows: size.rows });
                                    if let Some(ready) = self.ready.take() { let _ = ready.send(Ok(())); }
                                }
                                PtyEvent::Output { data } => {
                                    parser.advance(&mut screen, &data);
                                    // The Host is the sole terminal-query responder, even during disconnects.
                                    let output_replies=std::mem::take(&mut *replies.0.lock().unwrap());
                                    for reply in output_replies {
                                        use alacritty_terminal::event::{Event, WindowSize};
                                        use alacritty_terminal::vte::ansi::Rgb;
                                        let data = match reply {
                                            Event::PtyWrite(data)=>data,
                                            Event::ColorRequest(index,format)=> {
                                                let value=agent_protocol::operations::terminal_color(index as u16);
                                                let color=screen.colors()[index].unwrap_or(Rgb {r:(value>>16) as u8,g:(value>>8) as u8,b:value as u8});
                                                format(color)
                                            }
                                            Event::TextAreaSizeRequest(format)=>format(WindowSize {num_cols:screen.columns() as u16,num_lines:screen.screen_lines() as u16,cell_width:0,cell_height:0}),
                                            Event::ClipboardLoad(_,format)=>format(""),
                                            _=>unreachable!(),
                                        };
                                        query_bytes.extend_from_slice(data.as_bytes());
                                        if query_bytes.len() > agent_protocol::protocol::MAX_FRAME_BYTES { return Err("terminal query replies exceed the buffer limit".into()); }
                                    }
                                    persist_at.get_or_insert_with(|| tokio::time::Instant::now() + PERSIST_DELAY);
                                    self.channel.activity.fetch_add(1, Ordering::AcqRel);
                                    self.channel.observe(TerminalOutput::Data(data.clone()));
                                    self.publish(Notification::Output { handle: handle.clone(), data });
                                },
                                PtyEvent::Ack { id, error } => {
                                    if id == bex_process::TERMINAL_QUERY_REPLY_ID { query_in_flight=false; if let Some(error)=error {return Err(error);} continue; }
                                    if id == 0 { if let Some(error)=error {return Err(error);} continue; }
                                    let (expected, complete) = pending.take().ok_or("unexpected terminal acknowledgement")?;
                                    if expected != id { let _ = complete.send(Err("terminal acknowledgement ID changed".into())); return Err("terminal acknowledgement ID changed".into()); }
                                    let _ = complete.send(error.map_or(Ok(()), Err));
                                }
                                PtyEvent::Exited { code } => {
                                    let code = i32::try_from(code).unwrap_or(1);
                                    exit_code = Some(code);
                                    self.channel.observe(TerminalOutput::Exited);
                                    self.publish(Notification::Exited { handle: handle.clone(), code });
                                    return Ok(());
                                }
                                PtyEvent::Failed { message } => return Err(message),
                            }
                        }
                        command = self.input.recv(), if self.ready.is_none() && pending.is_none() => {
                            let Some(command) = command else { return Ok(()) };
                            match command.action {
                                Action::Attach(session, size) => {
                                    if screen.columns()!=usize::from(size.cols) || screen.screen_lines()!=usize::from(size.rows) {
                                        screen.resize(Dimensions(size));
                                        write(&mut stdin,&PtyCommand::Resize{id:0,rows:size.rows,cols:size.cols}).await?;
                                    }
                                    let mut data=screen.ansi_checkpoint(parser.preceding_char());
                                    data.extend(parser.checkpoint_tail());
                                    let result=self.channel.send(session, Notification::TerminalRestored { handle: handle.clone(), data, cols: screen.columns() as u16, rows: screen.screen_lines() as u16 });
                                    let _=command.complete.send(result);
                                    continue;
                                }
                                Action::Clear => {
                                    let size = TerminalSize { cols: screen.columns() as u16, rows: screen.screen_lines() as u16 };
                                    screen = new_screen(size, &replies);
                                    parser = Default::default();
                                    replies.0.lock().unwrap().clear();
                                    persist_at = None;
                                    let data = screen.ansi_checkpoint(None);
                                    self.saver.flush(data.clone()).await;
                                    self.publish(Notification::TerminalRestored { handle: handle.clone(), data, cols: size.cols, rows: size.rows });
                                    let _=command.complete.send(Ok(()));
                                    continue;
                                }
                                action => {
                                    next_id = next_id.checked_add(1).ok_or("terminal operation ID exhausted")?;
                                    if next_id == bex_process::TERMINAL_QUERY_REPLY_ID { return Err("terminal operation ID exhausted".into()); }
                                    let action = match action {
                                        Action::Write(data) => PtyCommand::Write {id:next_id,data},
                                        Action::Resize(size) => {screen.resize(Dimensions(size)); PtyCommand::Resize {id:next_id,rows:size.rows,cols:size.cols}},
                                        Action::Attach(..) | Action::Clear => unreachable!(),
                                    };
                                    pending = Some((next_id, command.complete));
                                    tokio::select! {
                                        _ = self.stop.cancelled() => return Ok(()),
                                        result = write(&mut stdin, &action) => result?,
                                    }
                                }
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
        self.saver.flush(screen.ansi_checkpoint(None)).await;
        let mut last = screen.ansi_checkpoint(parser.preceding_char());
        last.extend(parser.checkpoint_tail());
        let outcome = match result {
            Ok(()) => Outcome::Exited(exit_code),
            Err(message) => {
                self.channel.observe(TerminalOutput::Exited);
                self.publish(Notification::TerminalFailed {
                    handle: handle.clone(),
                    reason: message.clone(),
                });
                Outcome::Failed(message)
            }
        };
        (cleanup, outcome, last)
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

#[cfg(test)]
mod tests;
