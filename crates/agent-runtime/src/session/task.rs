use super::{
    Members, ProcessControl, ProviderProcess, SessionError, SessionManager, SessionOptions, Slot,
    claude::ClaudeProcess,
};
use crate::{ActorRegistry, RuntimeError};
use agent_domain::{
    EffectResult, Input, MessageId, ProviderEvent, ProviderItem, ProviderOperation, RunAttemptId,
    State, ThreadId,
};
use agent_providers::{
    ClaudeProtocol, CodexProtocol, Completion, ProtocolError, Translation, read_frame, write_frame,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// Commits to the owning actor are retried this many times before the session fails.
const COMMIT_ATTEMPTS: u32 = 8;
const COMMIT_BACKOFF: Duration = Duration::from_millis(50);
const COMMIT_BACKOFF_CAP: Duration = Duration::from_secs(1);
const TOKEN_REFRESH: &str = "account/chatgptAuthTokens/refresh";

pub(crate) enum Protocol {
    Codex(CodexProtocol),
    Claude(ClaudeProtocol),
}
impl Protocol {
    pub(crate) fn codex(&mut self) -> Result<&mut CodexProtocol, ProtocolError> {
        match self {
            Self::Codex(protocol) => Ok(protocol),
            Self::Claude(_) => Err(ProtocolError::Invalid("not a Codex session".into())),
        }
    }
    pub(crate) fn claude(&mut self) -> Result<&mut ClaudeProtocol, ProtocolError> {
        match self {
            Self::Claude(protocol) => Ok(protocol),
            Self::Codex(_) => Err(ProtocolError::Invalid("not a Claude session".into())),
        }
    }
    fn receive(&mut self, frame: &Value) -> Result<Translation, ProtocolError> {
        match self {
            Self::Codex(protocol) => protocol.receive(frame),
            Self::Claude(protocol) => protocol.receive(frame),
        }
    }
    /// Claude attributes a preceding turn in the state machine, so only Codex waits.
    fn turn_in_flight(&self, thread: &ThreadId) -> bool {
        match self {
            Self::Codex(protocol) => protocol.turn_in_flight(thread.as_str()),
            Self::Claude(_) => false,
        }
    }
}

pub(crate) type Op = Box<dyn FnOnce(&mut Protocol) -> Result<Translation, ProtocolError> + Send>;
pub(crate) type ReplyWait = oneshot::Receiver<Result<Value, String>>;
/// Request id and the caller waiting for its reply.
pub(crate) type ReplyWaiter = (String, oneshot::Sender<Result<Value, String>>);
pub(crate) type CompletionWait = oneshot::Receiver<Result<Completion, String>>;
type Written = oneshot::Sender<Result<(), String>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Expect {
    Written,
    /// A waiter for the reply to each outbound request.
    Replies,
    /// A waiter for the native operation's completion signal.
    Completion,
}

/// One translation run inside the session task.
pub(crate) struct Run {
    /// The app thread the run belongs to.
    pub(crate) thread: ThreadId,
    /// Becomes the thread's owner on this process before the translation runs.
    pub(crate) owner: Option<RunAttemptId>,
    /// The attempt that receives this translation's events; the owner by default.
    pub(crate) events_to: Option<RunAttemptId>,
    pub(crate) steer: Option<MessageId>,
    pub(crate) expect: Expect,
    /// Until a reply arrives, an exit is reported by the waiting caller rather
    /// than as a closed session of the owner.
    pub(crate) handshake: bool,
    pub(crate) op: Op,
    pub(crate) done: oneshot::Sender<Result<Ran, SessionError>>,
}
pub(crate) struct Ran {
    pub(crate) replies: Vec<ReplyWait>,
    pub(crate) completion: Option<CompletionWait>,
    /// Resolves once the run's frames are on the provider's stdin.
    pub(crate) written: Option<oneshot::Receiver<Result<(), String>>>,
}
pub(crate) enum Mail {
    Run(Run),
    /// Answered once no root turn of the thread is in flight, other than one
    /// of `attempt`.
    Settled {
        thread: ThreadId,
        attempt: Option<RunAttemptId>,
        done: oneshot::Sender<()>,
    },
    /// The thread no longer uses this process; its late output still reaches it.
    Detach {
        thread: ThreadId,
    },
    Close {
        report: bool,
        done: oneshot::Sender<()>,
    },
}

/// The thread, attempt (and steering message) a sent request belongs to.
struct Sent {
    thread: ThreadId,
    attempt: Option<RunAttemptId>,
    steer: Option<MessageId>,
}

/// One app thread's work on the process.
#[derive(Default)]
struct Member {
    owner: Option<RunAttemptId>,
    /// Every attempt that ran here, oldest first.
    attempts: Vec<RunAttemptId>,
    completion: Option<oneshot::Sender<Result<Completion, String>>>,
    settled: Vec<oneshot::Sender<()>>,
    detached: bool,
}
impl Member {
    fn remember(&mut self, attempt: &RunAttemptId) {
        if !self.attempts.contains(attempt) {
            self.attempts.push(attempt.clone());
        }
    }
}

struct Outgoing {
    frames: Vec<Value>,
    written: Option<Written>,
}

enum Event {
    Mail(Option<Mail>),
    Frame(std::io::Result<Option<Value>>),
    Stderr(std::io::Result<usize>),
    WriteFailed(String),
    Idle,
}
enum Exit {
    Closed {
        report: bool,
        done: Option<oneshot::Sender<()>>,
    },
    Eof,
    Idle,
    Abandoned,
    Failed(String),
}

pub(crate) struct Task {
    slot: Slot,
    instance: String,
    generation: u64,
    manager: Weak<SessionManager>,
    registry: Arc<ActorRegistry>,
    options: SessionOptions,
    protocol: Protocol,
    claude: Option<Arc<Mutex<ClaudeProcess>>>,
    attachments: Arc<Mutex<Members>>,
    mail: mpsc::UnboundedReceiver<Mail>,
    outgoing: mpsc::UnboundedSender<Outgoing>,
    writer: JoinHandle<()>,
    write_failed: Option<oneshot::Receiver<String>>,
    output: BufReader<Box<dyn AsyncRead + Send + Unpin>>,
    stderr: Option<Box<dyn AsyncRead + Send + Unpin>>,
    control: Box<dyn ProcessControl>,
    line: Vec<u8>,
    handshake: bool,
    members: BTreeMap<ThreadId, Member>,
    sent: HashMap<String, Sent>,
    /// Retained background work key or tool -> the thread and attempt that started it.
    background: HashMap<String, (ThreadId, RunAttemptId)>,
    replies: Vec<ReplyWaiter>,
    deadline: Instant,
    pinned_since: Option<Instant>,
    stderr_tail: String,
    failure: Option<String>,
}

impl Task {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        slot: Slot,
        instance: String,
        generation: u64,
        manager: Weak<SessionManager>,
        registry: Arc<ActorRegistry>,
        options: SessionOptions,
        protocol: Protocol,
        claude: Option<Arc<Mutex<ClaudeProcess>>>,
        attachments: Arc<Mutex<Members>>,
        mail: mpsc::UnboundedReceiver<Mail>,
        process: ProviderProcess,
    ) -> Self {
        let (outgoing, queue) = mpsc::unbounded_channel();
        let (failed, write_failed) = oneshot::channel();
        let writer = tokio::spawn(write_frames(
            process.input,
            queue,
            options.write_timeout,
            failed,
        ));
        Self {
            slot,
            instance,
            generation,
            manager,
            registry,
            deadline: Instant::now() + options.idle_timeout,
            options,
            protocol,
            claude,
            attachments,
            mail,
            outgoing,
            writer,
            write_failed: Some(write_failed),
            output: BufReader::new(process.output),
            stderr: Some(process.stderr),
            control: process.control,
            line: Vec::new(),
            handshake: false,
            members: BTreeMap::new(),
            sent: HashMap::new(),
            background: HashMap::new(),
            replies: Vec::new(),
            pinned_since: None,
            stderr_tail: String::new(),
            failure: None,
        }
    }

    pub(crate) async fn run(mut self) {
        let mut chunk = [0u8; 4096];
        let exit = loop {
            let deadline = self.deadline;
            let stderr = self.stderr.as_mut();
            let event = tokio::select! {
                mail = self.mail.recv() => Event::Mail(mail),
                frame = read_frame(&mut self.output, &mut self.line) => Event::Frame(frame),
                read = read_stderr(stderr, &mut chunk) => Event::Stderr(read),
                failed = write_failure(self.write_failed.as_mut()) => Event::WriteFailed(failed),
                () = tokio::time::sleep_until(deadline) => Event::Idle,
            };
            match event {
                Event::Mail(None) => break Exit::Abandoned,
                Event::Mail(Some(Mail::Run(run))) => {
                    self.touch();
                    self.run_op(run).await;
                }
                Event::Mail(Some(Mail::Settled {
                    thread,
                    attempt,
                    done,
                })) => {
                    let member = self.members.entry(thread).or_default();
                    if attempt.is_some() && member.owner == attempt {
                        let _ = done.send(());
                    } else {
                        member.settled.push(done);
                        self.release_settled();
                    }
                }
                Event::Mail(Some(Mail::Detach { thread })) => {
                    self.members.entry(thread).or_default().detached = true;
                }
                Event::Mail(Some(Mail::Close { report, done })) => {
                    break Exit::Closed {
                        report,
                        done: Some(done),
                    };
                }
                // Inbound traffic never postpones idle release or the pin cap.
                Event::Frame(Ok(Some(frame))) => {
                    self.frame(frame).await;
                    self.release_settled();
                }
                // A failed write closes stdin, so the provider may exit first.
                Event::Frame(Ok(None)) => match self.write_failed.as_mut().map(|f| f.try_recv()) {
                    Some(Ok(error)) => break Exit::Failed(error),
                    _ => break Exit::Eof,
                },
                Event::Frame(Err(error)) if error.kind() == std::io::ErrorKind::InvalidData => {
                    tracing::warn!(slot = ?self.slot, %error,
                        "skipping an undecodable provider frame");
                }
                Event::Frame(Err(error)) => {
                    tracing::warn!(slot = ?self.slot, %error, "provider output failed");
                    break Exit::Eof;
                }
                Event::Stderr(Ok(0)) | Event::Stderr(Err(_)) => self.stderr = None,
                Event::Stderr(Ok(read)) => self.keep_stderr(&chunk[..read]),
                Event::WriteFailed(error) => break Exit::Failed(error),
                Event::Idle => {
                    if self.idle_due().await {
                        break Exit::Idle;
                    }
                }
            }
            if let Some(error) = self.failure.take() {
                break Exit::Failed(error);
            }
        };
        self.finish(exit).await;
    }

    fn touch(&mut self) {
        self.deadline = Instant::now() + self.options.idle_timeout;
    }

    fn release_settled(&mut self) {
        for (thread, member) in &mut self.members {
            if !self.protocol.turn_in_flight(thread) {
                for waiter in member.settled.drain(..) {
                    let _ = waiter.send(());
                }
            }
        }
    }

    fn keep_stderr(&mut self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        tracing::debug!(slot = ?self.slot, stderr = %text);
        self.stderr_tail.push_str(&text);
        if self.stderr_tail.len() > 4096 {
            let mut cut = self.stderr_tail.len() - 4096;
            while !self.stderr_tail.is_char_boundary(cut) {
                cut += 1;
            }
            self.stderr_tail.drain(..cut);
        }
    }

    async fn run_op(&mut self, run: Run) {
        let Run {
            thread,
            owner,
            events_to,
            steer,
            expect,
            handshake,
            op,
            done,
        } = run;
        self.handshake |= handshake;
        let member = self.members.entry(thread.clone()).or_default();
        member.detached = false;
        if let Some(owner) = owner {
            member.remember(&owner);
            member.owner = Some(owner);
        }
        if let Some(attempt) = &events_to {
            member.remember(attempt);
        }
        let mut translation = match op(&mut self.protocol) {
            Ok(translation) => translation,
            Err(error) => {
                let _ = done.send(Err(SessionError::Protocol(error)));
                return;
            }
        };
        let (written, wait_written) = oneshot::channel();
        let mut ran = Ran {
            replies: vec![],
            completion: None,
            written: Some(wait_written),
        };
        match expect {
            Expect::Written => {}
            Expect::Replies => {
                for frame in &translation.outbound {
                    if let Some(id) = request(frame) {
                        let (sender, receiver) = oneshot::channel();
                        self.replies.push((id, sender));
                        ran.replies.push(receiver);
                    }
                }
            }
            Expect::Completion => {
                let (sender, receiver) = oneshot::channel();
                match translation.completion.take() {
                    Some(completion) => {
                        let _ = sender.send(Ok(completion));
                    }
                    None => {
                        let member = self.members.entry(thread.clone()).or_default();
                        if let Some(previous) = member.completion.replace(sender) {
                            let _ = previous.send(Err("superseded by a newer operation".into()));
                        }
                    }
                }
                ran.completion = Some(receiver);
            }
        }
        let result = self
            .deliver(translation, Some(thread), events_to, steer, Some(written))
            .await;
        let _ = done.send(result.map(|()| ran));
    }

    async fn frame(&mut self, frame: Value) {
        self.observe_frame(&frame);
        if frame["method"] == TOKEN_REFRESH && frame.get("id").is_some() {
            self.refresh_token(&frame);
            return;
        }
        let sent = reply_id(&frame)
            .and_then(|id| self.sent.get(&id))
            .map(|sent| (sent.thread.clone(), sent.attempt.clone()));
        match self.protocol.receive(&frame) {
            Ok(translation) => {
                let (thread, attempt) = match sent {
                    Some((thread, attempt)) => (Some(thread), attempt),
                    None => (None, None),
                };
                if let Err(error) = self.deliver(translation, thread, attempt, None, None).await {
                    tracing::warn!(slot = ?self.slot, %error, "provider frame was not fully handled");
                }
            }
            Err(error) => {
                let thread = sent.map(|(thread, _)| thread);
                self.protocol_error(error, thread).await
            }
        }
    }

    /// Answers the app-server's managed-token refresh with the selected account.
    fn refresh_token(&self, frame: &Value) {
        let Some(manager) = self.manager.upgrade() else {
            return;
        };
        let (id, instance, outgoing) = (
            frame["id"].clone(),
            self.instance.clone(),
            self.outgoing.clone(),
        );
        let previous = frame["params"]["previousAccountId"]
            .as_str()
            .map(str::to_owned);
        tokio::spawn(async move {
            let response = match manager.host.refresh_codex_account(instance, previous).await {
                Ok(result) => json!({"id":id,"result":result}),
                Err(message) => json!({"id":id,"error":{"code":-32000,"message":message}}),
            };
            let _ = outgoing.send(Outgoing {
                frames: vec![response],
                written: None,
            });
        });
    }

    /// The app thread a translation belongs to.
    fn thread_of(&self, translation: &Translation, known: Option<ThreadId>) -> Option<ThreadId> {
        known
            .or_else(|| {
                translation
                    .route
                    .as_deref()
                    .filter(|route| !route.is_empty())
                    .and_then(|route| ThreadId::new(route).ok())
            })
            .or_else(|| match &self.slot {
                Slot::Thread(key) => Some(key.thread.clone()),
                Slot::Shared(_) => None,
            })
    }

    /// Commits each event to the attempt that owns it, then writes outbound
    /// frames, then resolves replies: a native reply's facts are durable before
    /// the next outbound frame. A failed commit fails the session rather than
    /// leaving the translator ahead of the thread.
    async fn deliver(
        &mut self,
        translation: Translation,
        thread: Option<ThreadId>,
        attempt: Option<RunAttemptId>,
        steer: Option<MessageId>,
        written: Option<Written>,
    ) -> Result<(), SessionError> {
        let thread = self.thread_of(&translation, thread);
        let Translation {
            events,
            outbound,
            replies,
            completion,
            ..
        } = translation;
        let default = attempt.or_else(|| {
            thread
                .as_ref()
                .and_then(|thread| self.members.get(thread))
                .and_then(|member| member.owner.clone())
        });
        let mut carried = None;
        for event in events {
            // One account snapshot is kept per app-server and fills the
            // stopped turns of every thread it served.
            if matches!(event, ProviderEvent::RateLimits { .. }) {
                let served: Vec<_> = self
                    .members
                    .iter()
                    .filter(|(_, member)| !member.detached)
                    .filter_map(|(thread, member)| {
                        let attempt = member.owner.as_ref().or(member.attempts.last())?;
                        Some((thread.clone(), attempt.clone()))
                    })
                    .collect();
                for (thread, attempt) in served {
                    if let Err(error) = self.commit(&thread, &attempt, event.clone()).await {
                        self.failure =
                            Some(format!("Provider output could not be recorded: {error}"));
                        return Err(error.into());
                    }
                }
                continue;
            }
            let owner = match event_key(&event).and_then(|key| self.background.get(key)) {
                Some(owner) => {
                    carried = Some(owner.clone());
                    Some(owner.clone())
                }
                None if matches!(event, ProviderEvent::Wake { .. }) && carried.is_some() => {
                    carried.clone()
                }
                None => thread.clone().zip(default.clone()),
            };
            let Some((owner_thread, owner)) = owner else {
                tracing::debug!(slot = ?self.slot,
                    "dropping a provider event without a turn");
                continue;
            };
            self.observe(&event);
            self.retain(&event, &owner_thread, &owner);
            if let Err(error) = self.commit(&owner_thread, &owner, event).await {
                self.failure = Some(format!("Provider output could not be recorded: {error}"));
                return Err(error.into());
            }
        }
        if let Some(thread) = &thread {
            for frame in &outbound {
                if let Some(id) = request(frame) {
                    self.sent.insert(
                        id,
                        Sent {
                            thread: thread.clone(),
                            attempt: default.clone(),
                            steer: steer.clone(),
                        },
                    );
                }
            }
        }
        if outbound.is_empty() {
            if let Some(written) = written {
                let _ = written.send(Ok(()));
            }
        } else if let Err(rejected) = self.outgoing.send(Outgoing {
            frames: outbound,
            written,
        }) && let Some(written) = rejected.0.written
        {
            let _ = written.send(Err("The provider session closed.".into()));
        }
        for reply in replies {
            self.sent.remove(&reply.request);
            if let Some(index) = self.replies.iter().position(|(id, _)| *id == reply.request) {
                let (_, waiter) = self.replies.remove(index);
                self.handshake = false;
                let _ = waiter.send(Ok(reply.result.0));
            }
        }
        if let Some(completion) = completion
            && let Some(waiter) = thread
                .as_ref()
                .and_then(|thread| self.members.get_mut(thread))
                .and_then(|member| member.completion.take())
        {
            let _ = waiter.send(Ok(completion));
        }
        Ok(())
    }

    /// Background work, and a Codex tool that may outlive its turn, keep the
    /// attempt that started them.
    fn retain(&mut self, event: &ProviderEvent, thread: &ThreadId, attempt: &RunAttemptId) {
        match event {
            ProviderEvent::ItemStarted {
                key,
                kind: ProviderItem::Tool { input, .. },
            } if matches!(self.protocol, Protocol::Codex(_)) && input.0["persistent"] == true => {
                self.background
                    .insert(key.clone(), (thread.clone(), attempt.clone()));
            }
            ProviderEvent::ItemFinished {
                key,
                kind: ProviderItem::Tool { .. },
                ..
            } => {
                self.background.remove(key);
            }
            _ => {}
        }
        if let ProviderEvent::BackgroundTask {
            key, tool, status, ..
        } = event
        {
            if status.is_some_and(|status| status.terminal()) {
                self.background.remove(key);
                self.background.remove(tool);
            } else {
                let owner = (thread.clone(), attempt.clone());
                self.background.insert(key.clone(), owner.clone());
                self.background.insert(tool.clone(), owner);
            }
        }
    }

    fn observe(&self, event: &ProviderEvent) {
        let Some(claude) = &self.claude else {
            return;
        };
        match event {
            ProviderEvent::SessionReady { native_thread } => {
                claude.lock().expect("claude process").native = Some(native_thread.clone());
            }
            ProviderEvent::NativeOutput { events, .. } => {
                for event in events {
                    self.observe(event);
                }
            }
            _ => {}
        }
    }

    /// The CLI reports the permission mode it switched to itself (plan mode).
    fn observe_frame(&self, frame: &Value) {
        if let Some(claude) = &self.claude
            && frame["type"] == "system"
            && matches!(frame["subtype"].as_str(), Some("init" | "status"))
            && let Some(mode) = frame["permissionMode"].as_str()
        {
            claude.lock().expect("claude process").permission_mode = mode.into();
        }
    }

    async fn protocol_error(&mut self, error: ProtocolError, thread: Option<ThreadId>) {
        let thread = thread.or_else(|| match &self.slot {
            Slot::Thread(key) => Some(key.thread.clone()),
            Slot::Shared(_) => None,
        });
        match error {
            ProtocolError::Remote {
                request,
                operation,
                message,
                turn_completed,
            } => {
                let sent = request.as_ref().and_then(|id| self.sent.remove(id));
                if let Some(index) = self
                    .replies
                    .iter()
                    .position(|(id, _)| Some(id) == request.as_ref())
                {
                    let (_, waiter) = self.replies.remove(index);
                    let _ = waiter.send(Err(message));
                    return;
                }
                if completion_operation(&operation)
                    && let Some(waiter) = thread
                        .as_ref()
                        .and_then(|thread| self.members.get_mut(thread))
                        .and_then(|member| member.completion.take())
                {
                    let _ = waiter.send(Err(message));
                    return;
                }
                let Some((operation, session_lost)) = failed_operation(&operation) else {
                    tracing::warn!(slot = ?self.slot, %operation, %message,
                        "provider rejected an operation nobody waits for");
                    return;
                };
                let (thread, attempt, message_id) = match sent {
                    Some(sent) => (Some(sent.thread), sent.attempt, sent.steer),
                    None => {
                        let owner = thread
                            .as_ref()
                            .and_then(|thread| self.members.get(thread))
                            .and_then(|member| member.owner.clone());
                        (thread, owner, None)
                    }
                };
                let (Some(thread), Some(attempt)) = (thread, attempt) else {
                    return;
                };
                let failed = Input::Effect(EffectResult::ProviderFailed {
                    attempt,
                    operation,
                    message,
                    message_id: message_id.filter(|_| operation == ProviderOperation::Steer),
                    turn_completed,
                    session_lost,
                });
                if let Err(error) = self
                    .retrying(|task| task.input(&thread, failed.clone()))
                    .await
                {
                    tracing::warn!(slot = ?self.slot, %error, "could not record a provider failure");
                }
            }
            other => match thread
                .as_ref()
                .and_then(|thread| self.members.get_mut(thread))
                .and_then(|member| member.completion.take())
            {
                Some(waiter) => {
                    let _ = waiter.send(Err(other.to_string()));
                }
                None => {
                    tracing::warn!(slot = ?self.slot, error = %other, "invalid provider frame")
                }
            },
        }
    }

    async fn commit(
        &mut self,
        thread: &ThreadId,
        attempt: &RunAttemptId,
        event: ProviderEvent,
    ) -> Result<(), RuntimeError> {
        self.retrying(|task| task.provider(thread, attempt.clone(), event.clone()))
            .await
    }

    /// Retries a store failure with backoff; the step is pure, so a retry
    /// commits the same facts.
    async fn retrying<F, Fut>(&mut self, mut attempt: F) -> Result<(), RuntimeError>
    where
        F: FnMut(&Self) -> Fut,
        Fut: Future<Output = Result<(), RuntimeError>>,
    {
        let mut delay = COMMIT_BACKOFF;
        let mut tries = 1;
        loop {
            match attempt(self).await {
                Err(error @ (RuntimeError::Store(_) | RuntimeError::ActorStopped))
                    if tries < COMMIT_ATTEMPTS =>
                {
                    tracing::warn!(slot = ?self.slot, %error, tries,
                        "retrying a provider commit");
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(COMMIT_BACKOFF_CAP);
                    tries += 1;
                }
                result => return result,
            }
        }
    }

    fn provider(
        &self,
        thread: &ThreadId,
        attempt: RunAttemptId,
        event: ProviderEvent,
    ) -> impl Future<Output = Result<(), RuntimeError>> + use<> {
        let (registry, thread) = (self.registry.clone(), thread.clone());
        async move {
            let actor = registry.get_or_load(&thread).await?;
            actor.provider(attempt, event).await.map(|_| ())
        }
    }

    fn input(
        &self,
        thread: &ThreadId,
        input: Input,
    ) -> impl Future<Output = Result<(), RuntimeError>> + use<> {
        let (registry, thread) = (self.registry.clone(), thread.clone());
        async move {
            let actor = registry.get_or_load(&thread).await?;
            actor.input(input).await.map(|_| ())
        }
    }

    /// Busy while a turn of any thread runs on the process; background work
    /// pins an idle process up to `max_idle_pin`.
    async fn idle_due(&mut self) -> bool {
        if !self.mail.is_empty() {
            self.touch();
            return false;
        }
        let mut states = vec![];
        for (thread, member) in &self.members {
            match self.registry.state(thread).await {
                Ok(state) => states.push((state, member.attempts.clone())),
                Err(error) => {
                    tracing::warn!(%thread, %error, "cannot read the thread for idle release");
                    self.touch();
                    return false;
                }
            }
        }
        if !self.mail.is_empty() {
            self.touch();
            return false;
        }
        let activity = states
            .iter()
            .map(|(state, attempts)| activity(state, &self.instance, attempts))
            .max()
            .unwrap_or(Activity::Idle);
        match activity {
            Activity::Busy => {
                self.pinned_since = None;
                self.touch();
                false
            }
            Activity::Pinned => {
                let now = Instant::now();
                let since = *self.pinned_since.get_or_insert(now);
                if now.duration_since(since) < self.options.max_idle_pin {
                    self.deadline =
                        (now + self.options.idle_timeout).min(since + self.options.max_idle_pin);
                    false
                } else {
                    tracing::warn!(slot = ?self.slot,
                        "releasing an idle provider session whose background work outlived the pin");
                    true
                }
            }
            Activity::Idle => true,
        }
    }

    /// Closes stdin (a blocked write is abandoned), then waits a bounded time
    /// for the process to exit before killing it.
    async fn stop(&mut self) {
        let grace = self.options.close_grace;
        self.outgoing = mpsc::unbounded_channel().0;
        if tokio::time::timeout(grace, &mut self.writer).await.is_err() {
            self.writer.abort();
        }
        if tokio::time::timeout(grace, self.control.wait())
            .await
            .is_err()
        {
            self.control.kill().await;
        }
    }

    async fn finish(mut self, exit: Exit) {
        let (report, error, done) = match exit {
            Exit::Closed { report, done } => {
                self.stop().await;
                (report, None, done)
            }
            Exit::Idle => {
                self.stop().await;
                (true, None, None)
            }
            Exit::Abandoned => {
                self.stop().await;
                (false, None, None)
            }
            Exit::Failed(error) => {
                tracing::warn!(slot = ?self.slot, %error,
                    "closing a provider session that cannot continue");
                self.stop().await;
                (true, Some(error), None)
            }
            Exit::Eof => {
                self.writer.abort();
                let success =
                    match tokio::time::timeout(self.options.close_grace, self.control.wait()).await
                    {
                        Ok(success) => success,
                        Err(_) => {
                            self.control.kill().await;
                            false
                        }
                    };
                if !success {
                    tracing::warn!(slot = ?self.slot, stderr = %self.stderr_tail,
                        "provider process exited unsuccessfully");
                }
                // The stderr tail identifies why the provider exited.
                let stderr = self.stderr_tail.trim();
                let message = if stderr.is_empty() {
                    "Provider process exited".to_owned()
                } else {
                    format!("Provider process exited: {stderr}")
                };
                (true, (!success).then_some(message), None)
            }
        };
        for (_, waiter) in self.replies.drain(..) {
            let _ = waiter.send(Err("The provider session closed.".into()));
        }
        let mut closed = vec![];
        for (thread, member) in &mut self.members {
            if let Some(waiter) = member.completion.take() {
                let _ = waiter.send(Err("The provider session closed.".into()));
            }
            member.settled.clear();
            if !report || member.detached {
                continue;
            }
            // Every attempt that still owns work on this process learns it is gone;
            // the owner last, and not while its handshake reports the exit itself.
            let owner = member.owner.clone();
            closed.extend(
                member
                    .attempts
                    .iter()
                    .filter(|attempt| Some(*attempt) != owner.as_ref())
                    .map(|attempt| (thread.clone(), attempt.clone())),
            );
            if !self.handshake {
                closed.extend(owner.map(|owner| (thread.clone(), owner)));
            }
        }
        for (thread, attempt) in closed {
            let event = ProviderEvent::SessionClosed {
                error: error.clone(),
            };
            if let Err(error) = self.commit(&thread, &attempt, event).await {
                tracing::warn!(%thread, %error, "could not record a closed provider session");
            }
        }
        if let Some(manager) = self.manager.upgrade() {
            manager.closed(&self.slot, self.generation, &self.attachments);
        }
        if let Some(done) = done {
            let _ = done.send(());
        }
    }
}

/// Owns the provider's stdin so a provider that stops reading never blocks the
/// task that reads its output. A write that does not finish in time, or fails,
/// ends the session.
async fn write_frames(
    mut input: Box<dyn AsyncWrite + Send + Unpin>,
    mut queue: mpsc::UnboundedReceiver<Outgoing>,
    timeout: Duration,
    failed: oneshot::Sender<String>,
) {
    while let Some(Outgoing { frames, written }) = queue.recv().await {
        let mut result = Ok(());
        for frame in &frames {
            result = match tokio::time::timeout(timeout, write_frame(&mut input, frame)).await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(format!("provider stdin failed: {error}")),
                Err(_) => Err("The provider stopped reading its input.".to_owned()),
            };
            if result.is_err() {
                break;
            }
        }
        if let Some(written) = written {
            let _ = written.send(result.clone());
        }
        if let Err(error) = result {
            let _ = failed.send(error);
            return;
        }
    }
}

async fn write_failure(failed: Option<&mut oneshot::Receiver<String>>) -> String {
    match failed {
        Some(failed) => match failed.await {
            Ok(error) => error,
            Err(_) => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
}

async fn read_stderr(
    stderr: Option<&mut Box<dyn AsyncRead + Send + Unpin>>,
    chunk: &mut [u8],
) -> std::io::Result<usize> {
    match stderr {
        Some(stderr) => stderr.read(chunk).await,
        None => std::future::pending().await,
    }
}

/// The id of an outbound request whose reply can be awaited, as the
/// translators report it.
fn request(frame: &Value) -> Option<String> {
    if frame["type"] == "control_request" {
        return Some(frame["request_id"].as_str()?.to_owned());
    }
    frame.get("method")?;
    Some(frame.get("id")?.to_string())
}

/// The id of the request an inbound frame answers.
fn reply_id(frame: &Value) -> Option<String> {
    if frame["type"] == "control_response" {
        return Some(frame["response"]["request_id"].as_str()?.to_owned());
    }
    if frame.get("method").is_some() {
        return None;
    }
    Some(frame.get("id")?.to_string())
}

fn event_key(event: &ProviderEvent) -> Option<&str> {
    match event {
        ProviderEvent::ItemStarted { key, .. }
        | ProviderEvent::ItemFinished { key, .. }
        | ProviderEvent::TextDelta { key, .. }
        | ProviderEvent::BackgroundTask { key, .. } => Some(key),
        _ => None,
    }
}

fn completion_operation(operation: &str) -> bool {
    matches!(
        operation,
        "thread/read" | "thread/resume" | "thread/turns/list" | "thread/revert" | "thread/fork"
    )
}

/// The provider operation a rejected native request belongs to, and whether it
/// means the native session could not be resumed.
pub(crate) fn failed_operation(operation: &str) -> Option<(ProviderOperation, bool)> {
    Some(match operation {
        "thread/resume" => (ProviderOperation::Start, true),
        "initialize" | "thread/start" | "thread/inject_items" | "turn/start" => {
            (ProviderOperation::Start, false)
        }
        "turn/steer" => (ProviderOperation::Steer, false),
        "set_permission_mode" => (ProviderOperation::Start, false),
        "turn/interrupt"
        | "interrupt"
        | "thread/backgroundTerminals/terminate"
        | "thread/backgroundTerminals/list" => (ProviderOperation::Interrupt, false),
        "thread/compact/start" => (ProviderOperation::Compact, false),
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Activity {
    Idle,
    Pinned,
    Busy,
}

/// Running turns count as busy; a pending request alone does not keep the
/// session, and its release expires the request.
pub(crate) fn activity(state: &State, instance: &str, attempts: &[RunAttemptId]) -> Activity {
    if state
        .active_run()
        .is_some_and(|run| run.selection.instance == instance)
    {
        return Activity::Busy;
    }
    if holds_background(state, attempts) {
        Activity::Pinned
    } else {
        Activity::Idle
    }
}

/// Background commands, persistent tools or native tasks that run inside the
/// process of these attempts.
pub(crate) fn holds_background(state: &State, attempts: &[RunAttemptId]) -> bool {
    state
        .background_work
        .values()
        .any(|work| attempts.contains(&work.attempt))
        || state
            .tasks
            .iter()
            .any(|task| !task.status.terminal() && attempts.contains(&task.attempt))
        || state.items.iter().any(|item| {
            item.persistent_tool()
                && item
                    .attempt
                    .as_ref()
                    .is_some_and(|attempt| attempts.contains(attempt))
        })
}
