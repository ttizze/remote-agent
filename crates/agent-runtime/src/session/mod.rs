//! Provider processes, one per (thread, provider instance), driven by outbox effects.
mod claude;
mod codex;
mod effects;
mod process;
mod task;

pub use effects::*;
pub use process::*;

use crate::{ActorRegistry, KeyedSerial, Residency, RuntimeError};
use agent_domain::{
    Attachment, AttachmentKind, AttemptStatus, CommandId, Driver, EffectResult, InteractionMode,
    ModelSelection, NativeBinding, ProviderCommand, ProviderEvent, ProviderOperation, RunAttemptId,
    RuntimeMode, State, ThreadId, Workspace,
};
use agent_providers::{
    ClaudeLaunch, ClaudeProtocol, Completion, PreparedImage, ProcessDirective, ProtocolError,
    Translation, WireContext, claude_fork_session, claude_model_options,
    claude_runtime_query_policy,
};
use claude::ClaudeProcess;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use task::{Expect, Mail, Op, Protocol, Ran, Run, Task};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionKey {
    pub thread: ThreadId,
    pub instance: String,
}

/// What the Host needs to configure or launch a provider for one thread.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchTarget {
    pub key: SessionKey,
    pub selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub workspace: Option<Workspace>,
}

/// Host-owned parts of a Claude CLI launch.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaudeSettings {
    pub approval_policy: Option<String>,
    pub sandbox_kind: Option<String>,
    pub read_only_allows_global_reads: bool,
    pub additional_directories: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub mcp_servers: BTreeMap<String, Value>,
    pub settings: Option<Value>,
    pub extra_args: BTreeMap<String, Option<String>>,
    pub append_system_prompt: String,
    pub skills: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SpawnRequest {
    pub target: LaunchTarget,
    /// The Claude CLI launch; `None` starts the Codex app-server.
    pub claude: Option<ClaudeLaunch>,
}

/// Host I/O for provider sessions: spawning under the supervisor, launch
/// configuration (including MCP credentials) and attachment and transcript files.
pub trait SessionHost: Send + Sync {
    fn spawn(&self, request: SpawnRequest) -> BoxFuture<'_, io::Result<ProviderProcess>>;
    fn codex_context(&self, target: LaunchTarget) -> BoxFuture<'_, Result<WireContext, String>>;
    fn claude_settings(
        &self,
        target: LaunchTarget,
    ) -> BoxFuture<'_, Result<ClaudeSettings, String>>;
    /// Prepared bytes for the image attachments.
    fn images(
        &self,
        thread: ThreadId,
        attachments: Vec<Attachment>,
    ) -> BoxFuture<'_, Result<Vec<PreparedImage>, String>>;
    fn read_claude_session(
        &self,
        target: LaunchTarget,
        session: String,
    ) -> BoxFuture<'_, io::Result<String>>;
    fn write_claude_session(
        &self,
        target: LaunchTarget,
        session: String,
        transcript: String,
    ) -> BoxFuture<'_, io::Result<()>>;
    /// Called once a session's process is gone, for credential cleanup.
    fn released(&self, _key: &SessionKey, _revoke_credentials: bool) {}
    fn prompt_uuid(&self, effect_id: &str) -> String {
        derived_uuid("prompt", effect_id)
    }
    fn session_uuid(&self, attempt: &RunAttemptId) -> String {
        derived_uuid("session", attempt.as_str())
    }
}

pub fn derived_uuid(kind: &str, seed: &str) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!("{kind}:{seed}").as_bytes(),
    )
    .to_string()
}

#[derive(Debug, Clone)]
pub struct SessionOptions {
    pub idle_timeout: Duration,
    /// Background work keeps an idle session at most this long.
    pub max_idle_pin: Duration,
    pub reply_timeout: Duration,
    pub close_grace: Duration,
}
impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(30 * 60),
            max_idle_pin: Duration::from_secs(4 * 60 * 60),
            reply_timeout: Duration::from_secs(60),
            close_grace: Duration::from_secs(5),
        }
    }
}

/// Threads with a live provider session stay loaded.
#[derive(Clone, Default)]
pub struct LiveSessions(Arc<Mutex<HashMap<ThreadId, usize>>>);
impl LiveSessions {
    fn add(&self, thread: &ThreadId) {
        *self
            .0
            .lock()
            .expect("live sessions")
            .entry(thread.clone())
            .or_default() += 1;
    }
    fn remove(&self, thread: &ThreadId) {
        let mut live = self.0.lock().expect("live sessions");
        if let Some(count) = live.get_mut(thread) {
            *count -= 1;
            if *count == 0 {
                live.remove(thread);
            }
        }
    }
}
impl Residency for LiveSessions {
    fn pinned(&self, thread: &ThreadId) -> bool {
        self.0.lock().expect("live sessions").contains_key(thread)
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum SessionError {
    #[error("the provider session is no longer running")]
    Gone,
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("provider stdio failed: {0}")]
    Io(String),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}

/// How an effect execution ends when it does not simply succeed.
#[derive(Debug, Clone, PartialEq)]
pub enum ExecError {
    /// Transient; the outbox retries and maps the final failure.
    Retry(String),
    /// A definitive result for the thread.
    Settle(Box<EffectResult>),
}

enum Failure {
    /// The session closed before handling the request; open a new one.
    Gone,
    Exec(ExecError),
}
impl From<ExecError> for Failure {
    fn from(error: ExecError) -> Self {
        Self::Exec(error)
    }
}

#[derive(Clone)]
struct Entry {
    key: SessionKey,
    generation: u64,
    mail: mpsc::UnboundedSender<Mail>,
    claude: Option<Arc<Mutex<ClaudeProcess>>>,
}

#[derive(Default)]
struct Table {
    sessions: HashMap<SessionKey, Entry>,
    /// Attempt -> the session (and process generation) that ran it.
    routes: HashMap<RunAttemptId, (SessionKey, u64)>,
    generation: u64,
}

struct Request {
    owner: Option<RunAttemptId>,
    events_to: Option<RunAttemptId>,
    steer: Option<agent_domain::MessageId>,
    expect: Expect,
    handshake: bool,
    op: Op,
}
impl Request {
    fn new(
        op: impl FnOnce(&mut Protocol) -> Result<Translation, ProtocolError> + Send + 'static,
    ) -> Self {
        Self {
            owner: None,
            events_to: None,
            steer: None,
            expect: Expect::Written,
            handshake: false,
            op: Box::new(op),
        }
    }
    fn owner(mut self, attempt: &RunAttemptId) -> Self {
        self.owner = Some(attempt.clone());
        self
    }
    fn events_to(mut self, attempt: &RunAttemptId) -> Self {
        self.events_to = Some(attempt.clone());
        self
    }
    fn handshake(mut self) -> Self {
        self.handshake = true;
        self
    }
    fn expect(mut self, expect: Expect) -> Self {
        self.expect = expect;
        self
    }
}

fn frames(outbound: Vec<Value>) -> Translation {
    Translation {
        outbound,
        ..Translation::default()
    }
}

/// Owns every provider process. Frames are translated in the session's task,
/// committed to the owning actor, and only then answered.
pub struct SessionManager {
    registry: Arc<ActorRegistry>,
    host: Arc<dyn SessionHost>,
    options: SessionOptions,
    live: LiveSessions,
    table: Mutex<Table>,
    keys: KeyedSerial<SessionKey>,
    awaiting: AtomicUsize,
    me: Weak<SessionManager>,
}

impl SessionManager {
    pub fn new(
        registry: Arc<ActorRegistry>,
        host: Arc<dyn SessionHost>,
        options: SessionOptions,
        live: LiveSessions,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            registry,
            host,
            options,
            live,
            table: Mutex::new(Table::default()),
            keys: KeyedSerial::default(),
            awaiting: AtomicUsize::new(0),
            me: me.clone(),
        })
    }

    pub fn sessions(&self) -> Vec<SessionKey> {
        let mut keys: Vec<_> = self
            .table
            .lock()
            .expect("session table")
            .sessions
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    }

    pub fn is_live(&self, key: &SessionKey) -> bool {
        self.entry(key).is_some()
    }

    /// Executions waiting for a provider reply or completion.
    pub fn awaiting_provider(&self) -> usize {
        self.awaiting.load(Ordering::SeqCst)
    }

    /// Runs one process-bound provider effect of `thread`.
    pub async fn execute(
        &self,
        thread: &ThreadId,
        effect_id: &str,
        attempt: Option<&RunAttemptId>,
        command: &ProviderCommand,
    ) -> Result<(), ExecError> {
        let attempt =
            attempt.ok_or_else(|| ExecError::Retry("provider effect without an attempt".into()))?;
        match command {
            ProviderCommand::Start { .. } | ProviderCommand::Compact { .. } => {
                self.start(thread, effect_id, attempt, command).await
            }
            ProviderCommand::Steer { .. } => self.steer(thread, effect_id, attempt, command).await,
            ProviderCommand::Interrupt { .. } => self.interrupt(thread, attempt, command).await,
            ProviderCommand::Respond { .. } => self.respond(attempt, command).await,
            ProviderCommand::SetModel { .. } | ProviderCommand::SetRuntimeMode { .. } => {
                self.configure(attempt, command).await
            }
            ProviderCommand::Rollback { .. } | ProviderCommand::Fork { .. } => Err(
                ExecError::Retry("rollback and fork run through their own effects".into()),
            ),
        }
    }

    async fn start(
        &self,
        thread: &ThreadId,
        effect_id: &str,
        attempt: &RunAttemptId,
        command: &ProviderCommand,
    ) -> Result<(), ExecError> {
        let state = self.state(thread).await?;
        let target = start_target(&state, thread, attempt, command)?;
        let images = self.images(thread, command).await?;
        let prompt =
            (target.selection.driver == Driver::Claude).then(|| self.host.prompt_uuid(effect_id));
        self.keys
            .with_lock(target.key.clone(), async {
                for _ in 0..2 {
                    let started = match target.selection.driver {
                        Driver::Codex => self.start_codex(&target, attempt, command, &images).await,
                        Driver::Claude => {
                            self.start_claude(
                                &target,
                                &state,
                                attempt,
                                command,
                                prompt.clone().unwrap_or_default(),
                                &images,
                            )
                            .await
                        }
                    };
                    match started {
                        Ok(()) => return Ok(()),
                        Err(Failure::Exec(error)) => return Err(error),
                        Err(Failure::Gone) => {}
                    }
                }
                Err(ExecError::Retry(
                    "The provider session closed while the turn started.".into(),
                ))
            })
            .await
    }

    async fn steer(
        &self,
        thread: &ThreadId,
        effect_id: &str,
        attempt: &RunAttemptId,
        command: &ProviderCommand,
    ) -> Result<(), ExecError> {
        let ProviderCommand::Steer { message, .. } = command else {
            return Ok(());
        };
        let completed = |text: &str| {
            ExecError::Settle(Box::new(EffectResult::ProviderFailed {
                attempt: attempt.clone(),
                operation: ProviderOperation::Steer,
                message: text.into(),
                message_id: Some(message.clone()),
                turn_completed: true,
                session_lost: false,
            }))
        };
        let state = self.state(thread).await?;
        if attempt_finished(&state, attempt) {
            return Err(completed("The turn already completed."));
        }
        let Some(entry) = self.route(attempt) else {
            return Err(completed("The provider session is no longer running."));
        };
        let images = self.images(thread, command).await?;
        let prompt = if entry.claude.is_some() {
            self.host.prompt_uuid(effect_id)
        } else {
            String::new()
        };
        let command = command.clone();
        let mut request = Request::new(move |p| match p {
            Protocol::Codex(codex) => codex.command(&command, &WireContext::default(), &images),
            Protocol::Claude(claude) => claude.command(&command, &prompt, &images),
        })
        .events_to(attempt);
        request.steer = Some(message.clone());
        match self.send(&entry, request).await {
            Err(SessionError::Gone) => Err(completed("The provider session is no longer running.")),
            sent => settle_sent(sent, attempt, Some(ProviderOperation::Steer), Some(message))
                .map(|_| ())
                .map_err(|failure| match failure {
                    Failure::Exec(error) => error,
                    Failure::Gone => completed("The provider session is no longer running."),
                }),
        }
    }

    /// Without a live session for the attempt there is nothing left to stop, so
    /// the attempt learns that its session closed.
    async fn interrupt(
        &self,
        thread: &ThreadId,
        attempt: &RunAttemptId,
        command: &ProviderCommand,
    ) -> Result<(), ExecError> {
        let closed = || async {
            self.provider_event(
                thread,
                attempt,
                ProviderEvent::SessionClosed { error: None },
            )
            .await
            .map_err(|error| ExecError::Retry(error.to_string()))
        };
        let Some(entry) = self.route(attempt) else {
            return closed().await;
        };
        let command = command.clone();
        let sent = self
            .send(
                &entry,
                Request::new(move |p| match p {
                    Protocol::Codex(codex) => codex.command(&command, &WireContext::default(), &[]),
                    Protocol::Claude(claude) => claude.command(&command, "", &[]),
                })
                .events_to(attempt),
            )
            .await;
        match sent {
            Err(SessionError::Gone) => Ok(()),
            sent => unwrap_failure(settle_sent(
                sent,
                attempt,
                Some(ProviderOperation::Interrupt),
                None,
            )),
        }
    }

    async fn respond(
        &self,
        attempt: &RunAttemptId,
        command: &ProviderCommand,
    ) -> Result<(), ExecError> {
        let gone = || {
            ExecError::Settle(Box::new(EffectResult::ProviderFailed {
                attempt: attempt.clone(),
                operation: ProviderOperation::Respond,
                message: "The provider session is no longer running.".into(),
                message_id: None,
                turn_completed: false,
                session_lost: false,
            }))
        };
        let Some(entry) = self.route(attempt) else {
            return Err(gone());
        };
        let command = command.clone();
        let sent = self
            .send(
                &entry,
                Request::new(move |p| match p {
                    Protocol::Codex(codex) => codex.command(&command, &WireContext::default(), &[]),
                    Protocol::Claude(claude) => claude.command(&command, "", &[]),
                })
                .events_to(attempt),
            )
            .await;
        match sent {
            Err(SessionError::Gone) => Err(gone()),
            sent => unwrap_failure(settle_sent(
                sent,
                attempt,
                Some(ProviderOperation::Respond),
                None,
            )),
        }
    }

    /// Codex takes model and mode on each turn; a live Claude process is updated.
    async fn configure(
        &self,
        attempt: &RunAttemptId,
        command: &ProviderCommand,
    ) -> Result<(), ExecError> {
        let Some(entry) = self.route(attempt) else {
            return Ok(());
        };
        let Some(process) = entry.claude.clone() else {
            return Ok(());
        };
        let forwarded = command.clone();
        let sent = self
            .send(
                &entry,
                Request::new(move |p| p.claude()?.command(&forwarded, "", &[])).events_to(attempt),
            )
            .await;
        match sent {
            Err(SessionError::Gone) => Ok(()),
            Ok(_) => {
                let mut process = process.lock().expect("claude process");
                match command {
                    ProviderCommand::SetModel { selection } => {
                        process.model = claude_model_options(selection).model;
                    }
                    ProviderCommand::SetRuntimeMode {
                        runtime_mode,
                        interaction_mode,
                    } => {
                        process.permission_mode = agent_providers::claude_permission_mode(
                            *runtime_mode,
                            *interaction_mode,
                        )
                        .into();
                    }
                    _ => {}
                }
                Ok(())
            }
            sent => unwrap_failure(settle_sent(
                sent,
                attempt,
                Some(ProviderOperation::SetModel),
                None,
            )),
        }
    }

    /// The provider half of a rollback to an absolute head. `None` means the
    /// native session must be reset (`Input::NativeSessionReset`).
    pub async fn rollback(
        &self,
        thread: &ThreadId,
        instance: &str,
        command: &ProviderCommand,
    ) -> Result<Option<NativeBinding>, ExecError> {
        let ProviderCommand::Rollback {
            native_thread,
            absolute_head,
        } = command
        else {
            return Err(ExecError::Retry("not a provider rollback".into()));
        };
        let state = self.state(thread).await?;
        let target = instance_target(&state, thread, instance)?;
        let key = target.key.clone();
        self.keys
            .with_lock(key.clone(), async {
                if target.selection.driver == Driver::Claude {
                    let directive = ClaudeProtocol::default()
                        .command(command, "", &[])
                        .map_err(|error| ExecError::Retry(error.to_string()))?
                        .process;
                    if let Some(entry) = self.entry(&key) {
                        self.close_entry(&entry, true, false).await;
                    }
                    return match directive {
                        Some(ProcessDirective::Resume {
                            native_thread,
                            absolute_head,
                        }) => Ok(Some(NativeBinding {
                            instance: instance.into(),
                            thread: native_thread,
                            head: absolute_head,
                        })),
                        _ => Ok(None),
                    };
                }
                let context = self
                    .host
                    .codex_context(target.clone())
                    .await
                    .map_err(ExecError::Retry)?;
                let entry = match self.codex_session(&target, &context).await {
                    Ok(entry) => entry,
                    Err(Failure::Exec(error)) => return Err(error),
                    Err(Failure::Gone) => {
                        return Err(ExecError::Retry("The provider session closed.".into()));
                    }
                };
                let forwarded = command.clone();
                let completed = self
                    .request_completion(
                        &entry,
                        Request::new(move |p| p.codex()?.command(&forwarded, &context, &[])),
                    )
                    .await
                    .map_err(ExecError::Retry)?;
                match completed {
                    Completion::RolledBack { native_thread } => Ok(Some(NativeBinding {
                        instance: instance.into(),
                        thread: native_thread,
                        head: absolute_head.clone(),
                    })),
                    Completion::Forked { .. } => Err(ExecError::Retry(format!(
                        "Codex did not roll back {native_thread}"
                    ))),
                }
            })
            .await
    }

    /// Runs a `ForkNative` effect: the fork's native thread, or `ForkFailed`.
    pub async fn fork_native(
        &self,
        thread: &ThreadId,
        effect_id: &str,
        command: &CommandId,
        provider: &ProviderCommand,
    ) -> Result<Option<EffectResult>, ExecError> {
        let state = self.state(thread).await?;
        let Some(pending) = state.pending_forks.get(command) else {
            return Ok(None);
        };
        let failed = |message: String| {
            Ok(Some(EffectResult::ForkFailed {
                command: command.clone(),
                message,
            }))
        };
        let target = match instance_target(&state, thread, &pending.instance) {
            Ok(target) => target,
            Err(ExecError::Retry(message)) => return failed(message),
            Err(error) => return Err(error),
        };
        let forked = match target.selection.driver {
            Driver::Claude => self.fork_claude(&target, effect_id, provider).await,
            Driver::Codex => self.fork_codex(&target, provider).await,
        };
        match forked {
            Ok(native_thread) => Ok(Some(EffectResult::NativeForked {
                command: command.clone(),
                native_thread,
            })),
            Err(ForkError::Rejected(message)) => failed(message),
            Err(ForkError::Retry(message)) => Err(ExecError::Retry(message)),
        }
    }

    /// Closes the thread's sessions, as for archive or delete.
    pub async fn detach(&self, thread: &ThreadId, revoke_credentials: bool) {
        for entry in self.entries(|key| &key.thread == thread) {
            self.close_entry(&entry, true, revoke_credentials).await;
        }
    }

    /// Closes every session of a provider instance, as for sign-out.
    pub async fn close_instance(&self, instance: &str) {
        for entry in self.entries(|key| key.instance == instance) {
            self.close_entry(&entry, true, false).await;
        }
    }

    /// Host shutdown: recovery decides what the runs become, so no session
    /// closure is reported.
    pub async fn shutdown(&self) {
        for entry in self.entries(|_| true) {
            self.close_entry(&entry, false, false).await;
        }
    }

    fn entries(&self, filter: impl Fn(&SessionKey) -> bool) -> Vec<Entry> {
        self.table
            .lock()
            .expect("session table")
            .sessions
            .values()
            .filter(|entry| filter(&entry.key))
            .cloned()
            .collect()
    }

    fn entry(&self, key: &SessionKey) -> Option<Entry> {
        self.table
            .lock()
            .expect("session table")
            .sessions
            .get(key)
            .cloned()
    }

    fn route(&self, attempt: &RunAttemptId) -> Option<Entry> {
        let table = self.table.lock().expect("session table");
        let (key, generation) = table.routes.get(attempt)?;
        table
            .sessions
            .get(key)
            .filter(|entry| entry.generation == *generation)
            .cloned()
    }

    fn bind(&self, attempt: &RunAttemptId, entry: &Entry) {
        self.table
            .lock()
            .expect("session table")
            .routes
            .insert(attempt.clone(), (entry.key.clone(), entry.generation));
    }

    async fn spawn(
        &self,
        target: &LaunchTarget,
        launch: Option<ClaudeLaunch>,
    ) -> Result<Entry, ExecError> {
        if let Some(stale) = self.entry(&target.key) {
            self.close_entry(&stale, true, false).await;
        }
        let process = self
            .host
            .spawn(SpawnRequest {
                target: target.clone(),
                claude: launch.clone(),
            })
            .await
            .map_err(|error| ExecError::Retry(error.to_string()))?;
        let (mail, receiver) = mpsc::unbounded_channel();
        let claude = launch.map(|launch| {
            Arc::new(Mutex::new(ClaudeProcess {
                native: launch.native_session.clone().or(launch.new_session.clone()),
                model: launch.model.clone(),
                permission_mode: launch.policy.permission_mode.clone(),
                launch,
            }))
        });
        let entry = {
            let mut table = self.table.lock().expect("session table");
            table.generation += 1;
            let entry = Entry {
                key: target.key.clone(),
                generation: table.generation,
                mail,
                claude: claude.clone(),
            };
            table.sessions.insert(target.key.clone(), entry.clone());
            entry
        };
        self.live.add(&target.key.thread);
        let protocol = match target.selection.driver {
            Driver::Codex => Protocol::Codex(Default::default()),
            Driver::Claude => Protocol::Claude(Default::default()),
        };
        let task = Task {
            key: target.key.clone(),
            generation: entry.generation,
            manager: self.me.clone(),
            registry: self.registry.clone(),
            options: self.options.clone(),
            protocol,
            claude,
            mail: receiver,
            input: process.input,
            output: tokio::io::BufReader::new(process.output),
            stderr: Some(process.stderr),
            control: process.control,
            line: Vec::new(),
            owner: None,
            handshake: false,
            resuming: false,
            attempts: HashSet::new(),
            replies: Vec::new(),
            completion: None,
            steers: VecDeque::new(),
            deadline: tokio::time::Instant::now() + self.options.idle_timeout,
            pinned_since: None,
            stderr_tail: String::new(),
        };
        let (manager, key, generation) = (self.me.clone(), target.key.clone(), entry.generation);
        tokio::spawn(async move {
            if let Err(panic) = std::panic::AssertUnwindSafe(task.run())
                .catch_unwind()
                .await
            {
                tracing::error!(thread = %key.thread, instance = %key.instance, ?panic,
                    "provider session task panicked");
                if let Some(manager) = manager.upgrade() {
                    manager.closed(&key, generation, false);
                }
            }
        });
        Ok(entry)
    }

    /// Called by a session task once its process is gone.
    fn closed(&self, key: &SessionKey, generation: u64, revoke_credentials: bool) {
        {
            let mut table = self.table.lock().expect("session table");
            if table
                .sessions
                .get(key)
                .is_some_and(|entry| entry.generation == generation)
            {
                table.sessions.remove(key);
            }
            table
                .routes
                .retain(|_, (route, current)| !(route == key && *current == generation));
        }
        self.live.remove(&key.thread);
        self.host.released(key, revoke_credentials);
    }

    async fn close_entry(&self, entry: &Entry, report: bool, revoke: bool) {
        {
            let mut table = self.table.lock().expect("session table");
            if table
                .sessions
                .get(&entry.key)
                .is_some_and(|current| current.generation == entry.generation)
            {
                table.sessions.remove(&entry.key);
            }
        }
        let (done, closed) = oneshot::channel();
        let sent = entry.mail.send(Mail::Close {
            report,
            revoke,
            done,
        });
        if (sent.is_err() || closed.await.is_err()) && revoke {
            self.host.released(&entry.key, true);
        }
    }

    async fn send(&self, entry: &Entry, request: Request) -> Result<Ran, SessionError> {
        let (done, ran) = oneshot::channel();
        entry
            .mail
            .send(Mail::Run(Run {
                owner: request.owner,
                events_to: request.events_to,
                steer: request.steer,
                expect: request.expect,
                handshake: request.handshake,
                op: request.op,
                done,
            }))
            .map_err(|_| SessionError::Gone)?;
        ran.await.map_err(|_| SessionError::Gone)?
    }

    async fn request_reply(&self, entry: &Entry, request: Request) -> Result<Value, String> {
        let mut ran = self
            .send(entry, request.expect(Expect::Replies))
            .await
            .map_err(|error| error.to_string())?;
        let Some(reply) = ran.replies.pop() else {
            return Err("no request was sent".into());
        };
        self.wait(reply).await
    }

    async fn request_completion(
        &self,
        entry: &Entry,
        request: Request,
    ) -> Result<Completion, String> {
        let ran = self
            .send(entry, request.expect(Expect::Completion))
            .await
            .map_err(|error| error.to_string())?;
        let Some(completion) = ran.completion else {
            return Err("no operation was sent".into());
        };
        self.wait(completion).await
    }

    async fn wait<T>(&self, receiver: oneshot::Receiver<Result<T, String>>) -> Result<T, String> {
        self.awaiting.fetch_add(1, Ordering::SeqCst);
        let waited = tokio::time::timeout(self.options.reply_timeout, receiver).await;
        self.awaiting.fetch_sub(1, Ordering::SeqCst);
        match waited {
            Err(_) => Err("The provider did not reply in time.".into()),
            Ok(Err(_)) => Err("The provider session closed.".into()),
            Ok(Ok(result)) => result,
        }
    }

    async fn state(&self, thread: &ThreadId) -> Result<Arc<State>, ExecError> {
        self.registry
            .state(thread)
            .await
            .map_err(|error| ExecError::Retry(error.to_string()))
    }

    async fn images(
        &self,
        thread: &ThreadId,
        command: &ProviderCommand,
    ) -> Result<Vec<PreparedImage>, ExecError> {
        let attachments = match command {
            ProviderCommand::Start { attachments, .. }
            | ProviderCommand::Steer { attachments, .. } => attachments,
            _ => return Ok(vec![]),
        };
        if !attachments
            .iter()
            .any(|attachment| attachment.kind == AttachmentKind::Image)
        {
            return Ok(vec![]);
        }
        self.host
            .images(thread.clone(), attachments.clone())
            .await
            .map_err(ExecError::Retry)
    }

    async fn input(
        &self,
        thread: &ThreadId,
        input: agent_domain::Input,
    ) -> Result<(), RuntimeError> {
        let mut retried = false;
        loop {
            let actor = self.registry.get_or_load(thread).await?;
            match actor.input(input.clone()).await {
                Err(RuntimeError::ActorStopped) if !retried => retried = true,
                result => return result.map(|_| ()),
            }
        }
    }

    async fn provider_event(
        &self,
        thread: &ThreadId,
        attempt: &RunAttemptId,
        event: ProviderEvent,
    ) -> Result<(), RuntimeError> {
        let mut retried = false;
        loop {
            let actor = self.registry.get_or_load(thread).await?;
            match actor.provider(attempt.clone(), event.clone()).await {
                Err(RuntimeError::ActorStopped) if !retried => retried = true,
                result => return result.map(|_| ()),
            }
        }
    }
}

enum ForkError {
    Retry(String),
    /// The provider refused the fork; the child continues with portable context.
    Rejected(String),
}

fn operation(command: &ProviderCommand) -> Option<ProviderOperation> {
    Some(match command {
        ProviderCommand::Start { .. } => ProviderOperation::Start,
        ProviderCommand::Steer { .. } => ProviderOperation::Steer,
        ProviderCommand::Interrupt { .. } => ProviderOperation::Interrupt,
        ProviderCommand::Respond { .. } => ProviderOperation::Respond,
        ProviderCommand::Compact { .. } => ProviderOperation::Compact,
        ProviderCommand::SetModel { .. } | ProviderCommand::SetRuntimeMode { .. } => {
            ProviderOperation::SetModel
        }
        ProviderCommand::Rollback { .. } | ProviderCommand::Fork { .. } => return None,
    })
}

fn command_native(command: &ProviderCommand) -> Option<String> {
    match command {
        ProviderCommand::Start { native_thread, .. }
        | ProviderCommand::Compact { native_thread } => native_thread.clone(),
        _ => None,
    }
}

/// Maps a session reply to the effect outcome; protocol rejections become
/// `ProviderFailed` for the attempt.
fn settle_sent(
    sent: Result<Ran, SessionError>,
    attempt: &RunAttemptId,
    operation: Option<ProviderOperation>,
    message_id: Option<&agent_domain::MessageId>,
) -> Result<Ran, Failure> {
    match sent {
        Ok(ran) => Ok(ran),
        Err(SessionError::Gone) => Err(Failure::Gone),
        Err(SessionError::Protocol(error)) => {
            let (message, turn_completed) = match error {
                ProtocolError::Remote {
                    message,
                    turn_completed,
                    ..
                } => (message, turn_completed),
                other => (other.to_string(), false),
            };
            Err(ExecError::Settle(Box::new(EffectResult::ProviderFailed {
                attempt: attempt.clone(),
                operation: operation.unwrap_or(ProviderOperation::Start),
                message,
                message_id: message_id.cloned(),
                turn_completed,
                session_lost: false,
            }))
            .into())
        }
        Err(other) => Err(ExecError::Retry(other.to_string()).into()),
    }
}

fn unwrap_failure(result: Result<Ran, Failure>) -> Result<(), ExecError> {
    match result {
        Ok(_) | Err(Failure::Gone) => Ok(()),
        Err(Failure::Exec(error)) => Err(error),
    }
}

pub(crate) fn attempt_finished(state: &State, attempt: &RunAttemptId) -> bool {
    state
        .attempts
        .iter()
        .find(|candidate| &candidate.id == attempt)
        .is_some_and(|candidate| {
            matches!(
                candidate.status,
                AttemptStatus::Completed
                    | AttemptStatus::Interrupted
                    | AttemptStatus::Failed
                    | AttemptStatus::Cancelled
                    | AttemptStatus::Superseded
            )
        })
        || state
            .runs
            .iter()
            .find(|run| run.attempt.as_ref() == Some(attempt))
            .is_some_and(|run| run.status.terminal())
}

fn start_target(
    state: &State,
    thread: &ThreadId,
    attempt: &RunAttemptId,
    command: &ProviderCommand,
) -> Result<LaunchTarget, ExecError> {
    let current = state
        .thread
        .as_ref()
        .ok_or_else(|| ExecError::Retry(format!("thread {thread} does not exist")))?;
    let (selection, runtime_mode, interaction_mode) = match command {
        ProviderCommand::Start {
            selection,
            runtime_mode,
            interaction_mode,
            ..
        } => (selection.clone(), *runtime_mode, *interaction_mode),
        _ => (
            state
                .runs
                .iter()
                .find(|run| run.attempt.as_ref() == Some(attempt))
                .map_or_else(|| current.selection.clone(), |run| run.selection.clone()),
            current.runtime_mode,
            current.interaction_mode,
        ),
    };
    Ok(LaunchTarget {
        key: SessionKey {
            thread: thread.clone(),
            instance: selection.instance.clone(),
        },
        selection,
        runtime_mode,
        interaction_mode,
        workspace: current.workspace.clone(),
    })
}

fn instance_target(
    state: &State,
    thread: &ThreadId,
    instance: &str,
) -> Result<LaunchTarget, ExecError> {
    let current = state
        .thread
        .as_ref()
        .ok_or_else(|| ExecError::Retry(format!("thread {thread} does not exist")))?;
    let selection = if current.selection.instance == instance {
        current.selection.clone()
    } else {
        state
            .runs
            .iter()
            .rev()
            .find(|run| run.selection.instance == instance)
            .map(|run| run.selection.clone())
            .ok_or_else(|| {
                ExecError::Retry(format!(
                    "thread {thread} never ran provider instance {instance}"
                ))
            })?
    };
    Ok(LaunchTarget {
        key: SessionKey {
            thread: thread.clone(),
            instance: instance.into(),
        },
        selection,
        runtime_mode: current.runtime_mode,
        interaction_mode: current.interaction_mode,
        workspace: current.workspace.clone(),
    })
}

#[cfg(test)]
mod tests;
