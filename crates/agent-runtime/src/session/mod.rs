//! Provider processes driven by outbox effects: one Codex app-server per
//! provider instance shared by its threads, and one Claude CLI per thread.
mod claude;
mod codex;
mod effects;
mod process;
mod task;

pub use effects::*;
pub use process::*;

use crate::{ActorRegistry, KeyedSerial, Residency, RuntimeError};
use agent_domain::{
    Attachment, AttachmentKind, AttemptStatus, Driver, EffectResult, InteractionMode,
    ModelSelection, NativeBinding, ProviderCommand, ProviderEvent, ProviderOperation, Reply,
    RunAttemptId, RunStatus, RuntimeMode, State, ThreadId, TransferKind, Workspace,
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
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use task::{Expect, Mail, Op, Protocol, Ran, Run, Task, holds_background};
use tokio::sync::{mpsc, oneshot};

/// T3 bounds unloading a detached thread so a wedged provider cannot hold up
/// the thread's next attach.
const UNLOAD_TIMEOUT: Duration = Duration::from_secs(10);

/// A thread's use of a provider instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionKey {
    pub thread: ThreadId,
    pub instance: String,
}

/// A provider process: the Codex app-server an instance shares across threads
/// (T3 `supportsMultipleProviderThreadsPerSession`), or one thread's Claude CLI.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Slot {
    Shared(String),
    Thread(SessionKey),
}
impl Slot {
    fn of(key: &SessionKey, driver: Driver) -> Self {
        match driver {
            Driver::Codex => Self::Shared(key.instance.clone()),
            Driver::Claude => Self::Thread(key.clone()),
        }
    }
    fn instance(&self) -> &str {
        match self {
            Self::Shared(instance) => instance,
            Self::Thread(key) => &key.instance,
        }
    }
}

/// The threads a process serves and the credentials it was given.
#[derive(Default)]
pub(crate) struct Members {
    /// Attached thread -> its working directory.
    attached: BTreeMap<ThreadId, Option<String>>,
    /// Threads whose MCP credentials the process holds until it is gone.
    recorded: BTreeSet<ThreadId>,
    closed: bool,
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
    /// Tools of the app's MCP servers the CLI runs without asking (T3
    /// claudeMcpQueryOverrides), added to the policy's allowed tools.
    pub mcp_allowed_tools: Vec<String>,
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
    /// The thread's MCP credentials stop working; `None` revokes every instance's.
    fn revoke_credentials(&self, _thread: &ThreadId, _instance: Option<&str>) {}
    /// `account/login/start` parameters of the managed account a new Codex
    /// app-server of the instance signs in with; `None` keeps its own login.
    fn codex_account(&self, _instance: String) -> BoxFuture<'_, Result<Option<Value>, String>> {
        Box::pin(async { Ok(None) })
    }
    /// Answers the app-server's `account/chatgptAuthTokens/refresh`.
    fn refresh_codex_account(
        &self,
        _instance: String,
        _previous_account: Option<String>,
    ) -> BoxFuture<'_, Result<Value, String>> {
        Box::pin(async { Err("No managed Codex account is selected.".to_owned()) })
    }
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
    /// T3 waits this long for a stopped Claude turn before closing its query.
    pub interrupt_timeout: Duration,
    pub close_grace: Duration,
    /// A frame the provider does not accept on stdin within this ends the session.
    pub write_timeout: Duration,
}
impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(30 * 60),
            max_idle_pin: Duration::from_secs(4 * 60 * 60),
            reply_timeout: Duration::from_secs(60),
            interrupt_timeout: Duration::from_secs(10),
            close_grace: Duration::from_secs(5),
            write_timeout: Duration::from_secs(30),
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
    slot: Slot,
    generation: u64,
    mail: mpsc::UnboundedSender<Mail>,
    claude: Option<Arc<Mutex<ClaudeProcess>>>,
    members: Arc<Mutex<Members>>,
}
impl Entry {
    fn attached(&self, thread: &ThreadId) -> bool {
        self.members
            .lock()
            .expect("session members")
            .attached
            .contains_key(thread)
    }
}

#[derive(Default)]
struct Table {
    sessions: HashMap<Slot, Entry>,
    /// Attempt -> the process (and its generation) that ran it.
    routes: HashMap<RunAttemptId, (Slot, u64)>,
    generation: u64,
    /// Credentials handed to a process being prepared must survive another
    /// process's release (T3 MCP credential reservations).
    reserved: HashMap<SessionKey, usize>,
}

/// Holds a thread's credentials while its process is prepared.
struct Reservation<'a> {
    manager: &'a SessionManager,
    key: SessionKey,
}
impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        let mut table = self.manager.table.lock().expect("session table");
        if let Some(count) = table.reserved.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                table.reserved.remove(&self.key);
            }
        }
    }
}

struct Request {
    thread: ThreadId,
    owner: Option<RunAttemptId>,
    events_to: Option<RunAttemptId>,
    steer: Option<agent_domain::MessageId>,
    expect: Expect,
    handshake: bool,
    op: Op,
}
impl Request {
    fn new(
        thread: &ThreadId,
        op: impl FnOnce(&mut Protocol) -> Result<Translation, ProtocolError> + Send + 'static,
    ) -> Self {
        Self {
            thread: thread.clone(),
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

/// The wire context of a command that only needs its thread's route.
fn route_context(thread: &ThreadId) -> WireContext {
    WireContext {
        route: thread.as_str().to_owned(),
        ..WireContext::default()
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
    opening: KeyedSerial<Slot>,
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
            opening: KeyedSerial::default(),
            awaiting: AtomicUsize::new(0),
            me: me.clone(),
        })
    }

    /// The threads attached to live provider processes.
    pub fn sessions(&self) -> Vec<SessionKey> {
        let mut keys: Vec<_> = self
            .entries(|_| true)
            .into_iter()
            .flat_map(|entry| {
                let instance = entry.slot.instance().to_owned();
                let members = entry.members.lock().expect("session members");
                members
                    .attached
                    .keys()
                    .map(|thread| SessionKey {
                        thread: thread.clone(),
                        instance: instance.clone(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        keys.sort();
        keys
    }

    /// The working directories of the thread's own live provider processes.
    /// As in T3 CheckpointRestoreSafety, a shared app-server is left out: each
    /// turn runs in its thread's workspace, which the caller already checks.
    pub fn session_cwds(&self, thread: &ThreadId) -> Vec<String> {
        self.entries(|slot| matches!(slot, Slot::Thread(_)))
            .into_iter()
            .filter_map(|entry| {
                let members = entry.members.lock().expect("session members");
                members.attached.get(thread).cloned().flatten()
            })
            .collect()
    }

    pub fn is_live(&self, key: &SessionKey) -> bool {
        self.entries(|slot| slot.instance() == key.instance)
            .iter()
            .any(|entry| entry.attached(&key.thread))
    }

    /// The process count, for tests and diagnostics.
    pub fn processes(&self) -> usize {
        self.table.lock().expect("session table").sessions.len()
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
            ProviderCommand::Respond { .. } => self.respond(thread, attempt, command).await,
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
                let _reserved = self.reserve(&target.key);
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
        let (prompt, skills) = if entry.claude.is_some() {
            let target = start_target(&state, thread, attempt, command)?;
            let settings = self
                .host
                .claude_settings(target)
                .await
                .map_err(ExecError::Retry)?;
            (self.host.prompt_uuid(effect_id), settings.skills)
        } else {
            (String::new(), vec![])
        };
        let command = command.clone();
        let context = route_context(thread);
        let mut request = Request::new(thread, move |p| match p {
            Protocol::Codex(codex) => codex.command(&command, &context, &images),
            Protocol::Claude(claude) => {
                claude.set_skills(skills);
                claude.command(&command, &prompt, &images)
            }
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
        if entry.claude.is_some() {
            self.stop_claude(&entry, thread, attempt, command).await;
            return Ok(());
        }
        let command = command.clone();
        let context = route_context(thread);
        let sent = self
            .send(
                &entry,
                Request::new(thread, move |p| match p {
                    Protocol::Codex(codex) => codex.command(&command, &context, &[]),
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

    /// T3 ClaudeAdapterV2 interruptTurn: interrupt, then close the process, which
    /// also ends its background shells; the closure terminalizes the attempt.
    async fn stop_claude(
        &self,
        entry: &Entry,
        thread: &ThreadId,
        attempt: &RunAttemptId,
        command: &ProviderCommand,
    ) {
        let command = command.clone();
        let interrupted = self
            .request_reply_within(
                entry,
                Request::new(thread, move |p| p.claude()?.command(&command, "", &[]))
                    .events_to(attempt),
                self.options.interrupt_timeout,
            )
            .await;
        if let Err(message) = interrupted {
            tracing::debug!(%thread, %message, "Claude did not acknowledge the interrupt");
        }
        self.close_entry(entry, true).await;
    }

    async fn respond(
        &self,
        thread: &ThreadId,
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
        let context = route_context(thread);
        let sent = self
            .send(
                &entry,
                Request::new(thread, move |p| match p {
                    Protocol::Codex(codex) => codex.command(&command, &context, &[]),
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
                    if let Some(entry) = self.entry(&Slot::Thread(key.clone())) {
                        self.close_entry(&entry, true).await;
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
                let _reserved = self.reserve(&key);
                let (entry, context) = match self.codex_entry(&target).await {
                    Ok(opened) => opened,
                    Err(Failure::Exec(error)) => return Err(error),
                    Err(Failure::Gone) => {
                        return Err(ExecError::Retry("The provider session closed.".into()));
                    }
                };
                let turns = turns_after(&state, native_thread, absolute_head.as_deref());
                let (native, boundary) = (native_thread.clone(), absolute_head.clone());
                let completed = self
                    .request_completion(
                        &entry,
                        Request::new(thread, move |p| {
                            Ok(p.codex()?
                                .rollback(&native, turns, boundary.as_deref(), &context))
                        }),
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

    /// Runs a `ForkNative` effect for the attempt consuming a fork: the
    /// forked native thread, or `ForkFailed`.
    pub async fn fork_native(
        &self,
        thread: &ThreadId,
        effect_id: &str,
        attempt: Option<&RunAttemptId>,
        instance: &str,
        provider: &ProviderCommand,
    ) -> Result<Option<EffectResult>, ExecError> {
        let Some(attempt) = attempt else {
            return Ok(None);
        };
        let state = self.state(thread).await?;
        let failed = |message: String| {
            Ok(Some(EffectResult::ForkFailed {
                attempt: attempt.clone(),
                message,
            }))
        };
        let target = match instance_target(&state, thread, instance) {
            Ok(target) => target,
            Err(ExecError::Retry(message)) => return failed(message),
            Err(error) => return Err(error),
        };
        let forked = match target.selection.driver {
            Driver::Claude => {
                let Some(source) = state.transfers.iter().find(|transfer| {
                    transfer.kind == TransferKind::Fork
                        && !transfer.superseded
                        && &transfer.target == thread
                        && transfer.native_source.is_some()
                }) else {
                    return failed("The fork has no native source.".into());
                };
                let source_state = self.state(&source.source).await?;
                let source = match instance_target(&source_state, &source.source, instance) {
                    Ok(source) => source,
                    Err(ExecError::Retry(message)) => return failed(message),
                    Err(error) => return Err(error),
                };
                self.fork_claude(&source, &target, effect_id, attempt, provider)
                    .await
            }
            Driver::Codex => self.fork_codex(&target, provider).await.map(Some),
        };
        match forked {
            Ok(None) => Ok(None),
            Ok(Some(native_thread)) => Ok(Some(EffectResult::NativeForked {
                attempt: attempt.clone(),
                native_thread,
            })),
            Err(ForkError::Rejected(message)) => failed(message),
            Err(ForkError::Retry(message)) => Err(ExecError::Retry(message)),
        }
    }

    /// Detaches the thread from its provider processes, as for archive, delete
    /// or settle (T3 ProviderSessionManager.detach). A thread's own Claude
    /// process closes; the shared Codex app-server interrupts the thread's turn,
    /// unloads its native thread and stays up for other threads until idle.
    /// Terminal detaches revoke the thread's credentials even without a process.
    pub async fn detach(&self, thread: &ThreadId, revoke_credentials: bool) {
        self.detach_instance(thread, None, revoke_credentials).await;
    }

    /// `detach` for one provider instance, as T3 releases the previous
    /// instance's session after a provider switch.
    pub async fn detach_instance(
        &self,
        thread: &ThreadId,
        instance: Option<&str>,
        revoke_credentials: bool,
    ) {
        for entry in self.entries(|slot| instance.is_none_or(|i| slot.instance() == i)) {
            if !entry.attached(thread) {
                continue;
            }
            match entry.slot.clone() {
                Slot::Thread(_) => self.close_entry(&entry, true).await,
                Slot::Shared(instance) => {
                    let key = SessionKey {
                        thread: thread.clone(),
                        instance,
                    };
                    self.keys
                        .with_lock(key, self.detach_shared(&entry, thread))
                        .await
                }
            }
        }
        if revoke_credentials {
            for entry in self.entries(|slot| instance.is_none_or(|i| slot.instance() == i)) {
                entry
                    .members
                    .lock()
                    .expect("session members")
                    .recorded
                    .remove(thread);
            }
            self.host.revoke_credentials(thread, instance);
        }
    }

    async fn detach_shared(&self, entry: &Entry, thread: &ThreadId) {
        let route = thread.as_str().to_owned();
        let interrupt = self.send(
            entry,
            Request::new(thread, move |p| {
                Ok(p.codex()?.interrupt_active_turn(&route))
            }),
        );
        match tokio::time::timeout(UNLOAD_TIMEOUT, interrupt).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(%thread, %error, "could not interrupt a detached thread's turn");
            }
            Err(_) => tracing::warn!(%thread, "interrupting a detached thread timed out"),
        }
        let detached = {
            let mut members = entry.members.lock().expect("session members");
            members.attached.remove(thread).is_some()
        };
        if detached {
            self.live.remove(thread);
        }
        let route = thread.as_str().to_owned();
        let unload = self.send(
            entry,
            Request::new(thread, move |p| Ok(p.codex()?.unload(&route))).expect(Expect::Replies),
        );
        match tokio::time::timeout(UNLOAD_TIMEOUT, async {
            let mut ran = unload.await.map_err(|error| error.to_string())?;
            match ran.replies.pop() {
                Some(reply) => self.wait(reply).await.map(|_| ()),
                None => Ok(()),
            }
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(%thread, %error, "could not unload a detached thread");
            }
            Err(_) => tracing::warn!(%thread, "unloading a detached thread timed out"),
        }
        let _ = entry.mail.send(Mail::Detach {
            thread: thread.clone(),
        });
    }

    /// Closes every process of a provider instance, as for sign-out.
    pub async fn close_instance(&self, instance: &str) {
        for entry in self.entries(|slot| slot.instance() == instance) {
            self.close_entry(&entry, true).await;
        }
    }

    /// Signs the instance's live Codex app-server in with the selected managed
    /// account, as the Host's single app-server did when an account was selected.
    pub async fn apply_codex_account(&self, instance: &str) -> Result<(), String> {
        let Some(entry) = self.entry(&Slot::Shared(instance.to_owned())) else {
            return Ok(());
        };
        let Some(params) = self.host.codex_account(instance.to_owned()).await? else {
            return Ok(());
        };
        let thread = {
            let members = entry.members.lock().expect("session members");
            members
                .attached
                .keys()
                .chain(&members.recorded)
                .next()
                .cloned()
        };
        let Some(thread) = thread else {
            return Ok(());
        };
        self.request_reply(
            &entry,
            Request::new(&thread, move |p| Ok(frames(vec![p.codex()?.login(params)]))),
        )
        .await
        .map(|_| ())
    }

    /// Host shutdown: recovery decides what the runs become, so no session
    /// closure is reported.
    pub async fn shutdown(&self) {
        for entry in self.entries(|_| true) {
            self.close_entry(&entry, false).await;
        }
    }

    fn entries(&self, filter: impl Fn(&Slot) -> bool) -> Vec<Entry> {
        self.table
            .lock()
            .expect("session table")
            .sessions
            .values()
            .filter(|entry| filter(&entry.slot))
            .cloned()
            .collect()
    }

    fn entry(&self, slot: &Slot) -> Option<Entry> {
        self.table
            .lock()
            .expect("session table")
            .sessions
            .get(slot)
            .cloned()
    }

    fn route(&self, attempt: &RunAttemptId) -> Option<Entry> {
        let table = self.table.lock().expect("session table");
        let (slot, generation) = table.routes.get(attempt)?;
        table
            .sessions
            .get(slot)
            .filter(|entry| entry.generation == *generation)
            .cloned()
    }

    /// The attempts whose work runs in this entry's process.
    fn entry_attempts(&self, entry: &Entry) -> Vec<RunAttemptId> {
        self.table
            .lock()
            .expect("session table")
            .routes
            .iter()
            .filter(|(_, (slot, generation))| {
                *slot == entry.slot && *generation == entry.generation
            })
            .map(|(attempt, _)| attempt.clone())
            .collect()
    }

    fn reserve(&self, key: &SessionKey) -> Reservation<'_> {
        *self
            .table
            .lock()
            .expect("session table")
            .reserved
            .entry(key.clone())
            .or_default() += 1;
        Reservation {
            manager: self,
            key: key.clone(),
        }
    }

    /// The thread uses the process; `Gone` once the process is closing.
    fn attach(&self, entry: &Entry, target: &LaunchTarget) -> Result<(), Failure> {
        let thread = &target.key.thread;
        let mut members = entry.members.lock().expect("session members");
        if members.closed {
            return Err(Failure::Gone);
        }
        members.recorded.insert(thread.clone());
        let cwd = target
            .workspace
            .as_ref()
            .map(|workspace| workspace.cwd.clone());
        if members.attached.insert(thread.clone(), cwd).is_none() {
            self.live.add(thread);
        }
        Ok(())
    }

    /// T3 ProviderTurnStartService: after any preparation, the attempt must
    /// still be the run's current one right before its turn is sent.
    async fn still_current(
        &self,
        thread: &ThreadId,
        attempt: &RunAttemptId,
    ) -> Result<bool, ExecError> {
        let state = self.state(thread).await?;
        Ok(!attempt_finished(&state, attempt)
            && state.runs.iter().any(|run| {
                run.attempt.as_ref() == Some(attempt)
                    && matches!(run.status, RunStatus::Starting | RunStatus::Running)
            }))
    }

    /// Waits (bounded, as T3 interruptAndAwaitTerminal) until the thread's
    /// previous root turn on the process ends, so its late events stay with its
    /// own attempt.
    async fn settled(
        &self,
        entry: &Entry,
        thread: &ThreadId,
        attempt: &RunAttemptId,
    ) -> Result<(), Failure> {
        let (done, settled) = oneshot::channel();
        entry
            .mail
            .send(Mail::Settled {
                thread: thread.clone(),
                attempt: attempt.clone(),
                done,
            })
            .map_err(|_| Failure::Gone)?;
        match tokio::time::timeout(self.options.reply_timeout, settled).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(Failure::Gone),
            Err(_) => Err(ExecError::Retry(
                "The previous provider turn did not finish before the next one started.".into(),
            )
            .into()),
        }
    }

    fn bind(&self, attempt: &RunAttemptId, entry: &Entry) {
        self.table
            .lock()
            .expect("session table")
            .routes
            .insert(attempt.clone(), (entry.slot.clone(), entry.generation));
    }

    /// Starts the process for the target's slot; the target's thread is attached.
    async fn spawn(
        &self,
        target: &LaunchTarget,
        launch: Option<ClaudeLaunch>,
    ) -> Result<Entry, Failure> {
        let slot = Slot::of(&target.key, target.selection.driver);
        if let Some(stale) = self.entry(&slot) {
            self.close_entry(&stale, true).await;
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
                permission_mode: launch.policy.permission_mode.clone(),
                launch,
            }))
        });
        let members = Arc::new(Mutex::new(Members::default()));
        let entry = {
            let mut table = self.table.lock().expect("session table");
            table.generation += 1;
            let entry = Entry {
                slot: slot.clone(),
                generation: table.generation,
                mail,
                claude: claude.clone(),
                members: members.clone(),
            };
            table.sessions.insert(slot.clone(), entry.clone());
            entry
        };
        self.attach(&entry, target)?;
        let protocol = match target.selection.driver {
            Driver::Codex => Protocol::Codex(Default::default()),
            Driver::Claude => Protocol::Claude(Default::default()),
        };
        let task = Task::new(
            slot.clone(),
            target.key.instance.clone(),
            entry.generation,
            self.me.clone(),
            self.registry.clone(),
            self.options.clone(),
            protocol,
            claude,
            members.clone(),
            receiver,
            process,
        );
        let (manager, generation) = (self.me.clone(), entry.generation);
        tokio::spawn(async move {
            if let Err(panic) = std::panic::AssertUnwindSafe(task.run())
                .catch_unwind()
                .await
            {
                tracing::error!(?slot, ?panic, "provider session task panicked");
                if let Some(manager) = manager.upgrade() {
                    manager.closed(&slot, generation, &members);
                }
            }
        });
        Ok(entry)
    }

    /// Called by a session task once its process is gone. Every credential the
    /// process recorded is revoked unless another process holds it or a process
    /// being prepared reserved it (T3 releaseEntry).
    fn closed(&self, slot: &Slot, generation: u64, members: &Mutex<Members>) {
        {
            let mut table = self.table.lock().expect("session table");
            if table
                .sessions
                .get(slot)
                .is_some_and(|entry| entry.generation == generation)
            {
                table.sessions.remove(slot);
            }
            table
                .routes
                .retain(|_, (route, current)| !(route == slot && *current == generation));
        }
        let (attached, recorded) = {
            let mut members = members.lock().expect("session members");
            members.closed = true;
            (
                std::mem::take(&mut members.attached),
                std::mem::take(&mut members.recorded),
            )
        };
        for thread in attached.keys() {
            self.live.remove(thread);
        }
        let instance = slot.instance();
        for thread in recorded {
            let key = SessionKey {
                thread: thread.clone(),
                instance: instance.to_owned(),
            };
            let held = self
                .table
                .lock()
                .expect("session table")
                .reserved
                .contains_key(&key)
                || self
                    .entries(|other| other.instance() == instance)
                    .iter()
                    .any(|other| {
                        other
                            .members
                            .lock()
                            .expect("session members")
                            .recorded
                            .contains(&thread)
                    });
            if !held {
                self.host.revoke_credentials(&thread, Some(instance));
            }
        }
    }

    async fn close_entry(&self, entry: &Entry, report: bool) {
        {
            let mut table = self.table.lock().expect("session table");
            if table
                .sessions
                .get(&entry.slot)
                .is_some_and(|current| current.generation == entry.generation)
            {
                table.sessions.remove(&entry.slot);
            }
        }
        let (done, closed) = oneshot::channel();
        if entry.mail.send(Mail::Close { report, done }).is_ok() {
            let _ = closed.await;
        }
    }

    /// Runs the request in the session task and waits until its frames are written.
    async fn send(&self, entry: &Entry, request: Request) -> Result<Ran, SessionError> {
        let (done, ran) = oneshot::channel();
        entry
            .mail
            .send(Mail::Run(Run {
                thread: request.thread,
                owner: request.owner,
                events_to: request.events_to,
                steer: request.steer,
                expect: request.expect,
                handshake: request.handshake,
                op: request.op,
                done,
            }))
            .map_err(|_| SessionError::Gone)?;
        let mut ran = ran.await.map_err(|_| SessionError::Gone)??;
        if let Some(written) = ran.written.take() {
            match written.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => return Err(SessionError::Io(error)),
                Err(_) => return Err(SessionError::Gone),
            }
        }
        Ok(ran)
    }

    async fn request_reply(&self, entry: &Entry, request: Request) -> Result<Value, String> {
        self.request_reply_within(entry, request, self.options.reply_timeout)
            .await
    }

    async fn request_reply_within(
        &self,
        entry: &Entry,
        request: Request,
        timeout: Duration,
    ) -> Result<Value, String> {
        let mut ran = self
            .send(entry, request.expect(Expect::Replies))
            .await
            .map_err(|error| error.to_string())?;
        let Some(reply) = ran.replies.pop() else {
            return Err("no request was sent".into());
        };
        self.wait_within(reply, timeout).await
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
        self.wait_within(receiver, self.options.reply_timeout).await
    }

    async fn wait_within<T>(
        &self,
        receiver: oneshot::Receiver<Result<T, String>>,
        timeout: Duration,
    ) -> Result<T, String> {
        self.awaiting.fetch_add(1, Ordering::SeqCst);
        let waited = tokio::time::timeout(timeout, receiver).await;
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
    ) -> Result<Reply, RuntimeError> {
        let mut retried = false;
        loop {
            let actor = self.registry.get_or_load(thread).await?;
            match actor.input(input.clone()).await {
                Err(RuntimeError::ActorStopped) if !retried => retried = true,
                result => return result.map(|committed| committed.reply),
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

/// T3 countTerminalTurnsAfterBoundary: the native thread's terminal turns after
/// the one that ended at `head`. A head the thread's own turns do not hold (a
/// fork's inherited boundary) or no head discards every turn of the thread.
fn turns_after(state: &State, native_thread: &str, head: Option<&str>) -> u64 {
    let turns: Vec<_> = state
        .attempts
        .iter()
        .filter(|attempt| {
            attempt.native_thread.as_deref() == Some(native_thread)
                && attempt.native_turn.is_some()
                && state
                    .runs
                    .iter()
                    .find(|run| run.id == attempt.run)
                    .is_some_and(|run| run.status != RunStatus::RolledBack)
        })
        .collect();
    let after = head
        .and_then(|head| {
            turns
                .iter()
                .position(|attempt| attempt.native_head.as_deref() == Some(head))
        })
        .map_or(0, |boundary| boundary + 1);
    turns[after..]
        .iter()
        .filter(|attempt| {
            matches!(
                attempt.status,
                AttemptStatus::Completed
                    | AttemptStatus::Interrupted
                    | AttemptStatus::Failed
                    | AttemptStatus::Cancelled
                    | AttemptStatus::Superseded
            )
        })
        .count() as u64
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
pub(crate) mod tests;
