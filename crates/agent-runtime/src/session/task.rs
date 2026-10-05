use super::{
    ProcessControl, SessionError, SessionKey, SessionManager, SessionOptions, claude::ClaudeProcess,
};
use crate::{ActorRegistry, RuntimeError};
use agent_domain::{
    EffectResult, Input, MessageId, ProviderEvent, ProviderOperation, RequestStatus, RunAttemptId,
    State,
};
use agent_providers::{
    ClaudeProtocol, CodexProtocol, Completion, ProtocolError, Translation, read_frame, write_frame,
};
use serde_json::Value;
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

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
}

pub(crate) type Op = Box<dyn FnOnce(&mut Protocol) -> Result<Translation, ProtocolError> + Send>;
pub(crate) type ReplyWait = oneshot::Receiver<Result<Value, String>>;
/// Request id, operation and the caller waiting for the reply.
pub(crate) type ReplyWaiter = (String, String, oneshot::Sender<Result<Value, String>>);
pub(crate) type CompletionWait = oneshot::Receiver<Result<Completion, String>>;

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
    /// Becomes the session owner before the translation runs.
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
}
pub(crate) enum Mail {
    Run(Run),
    Close {
        report: bool,
        revoke: bool,
        done: oneshot::Sender<()>,
    },
}

enum Event {
    Mail(Option<Mail>),
    Frame(std::io::Result<Option<Value>>),
    Stderr(std::io::Result<usize>),
    Idle,
}
enum Exit {
    Closed {
        report: bool,
        revoke: bool,
        done: Option<oneshot::Sender<()>>,
    },
    Eof,
    Idle,
    Abandoned,
}

pub(crate) struct Task {
    pub(crate) key: SessionKey,
    pub(crate) generation: u64,
    pub(crate) manager: Weak<SessionManager>,
    pub(crate) registry: Arc<ActorRegistry>,
    pub(crate) options: SessionOptions,
    pub(crate) protocol: Protocol,
    pub(crate) claude: Option<Arc<Mutex<ClaudeProcess>>>,
    pub(crate) mail: mpsc::UnboundedReceiver<Mail>,
    pub(crate) input: Box<dyn AsyncWrite + Send + Unpin>,
    pub(crate) output: BufReader<Box<dyn AsyncRead + Send + Unpin>>,
    pub(crate) stderr: Option<Box<dyn AsyncRead + Send + Unpin>>,
    pub(crate) control: Box<dyn ProcessControl>,
    pub(crate) line: Vec<u8>,
    pub(crate) owner: Option<RunAttemptId>,
    pub(crate) handshake: bool,
    pub(crate) attempts: HashSet<RunAttemptId>,
    pub(crate) replies: Vec<ReplyWaiter>,
    pub(crate) completion: Option<oneshot::Sender<Result<Completion, String>>>,
    pub(crate) steers: VecDeque<Option<MessageId>>,
    pub(crate) deadline: Instant,
    pub(crate) pinned_since: Option<Instant>,
    pub(crate) stderr_tail: String,
}

impl Task {
    pub(crate) async fn run(mut self) {
        let mut chunk = [0u8; 4096];
        let exit = loop {
            let deadline = self.deadline;
            let stderr = self.stderr.as_mut();
            let event = tokio::select! {
                mail = self.mail.recv() => Event::Mail(mail),
                frame = read_frame(&mut self.output, &mut self.line) => Event::Frame(frame),
                read = read_stderr(stderr, &mut chunk) => Event::Stderr(read),
                () = tokio::time::sleep_until(deadline) => Event::Idle,
            };
            match event {
                Event::Mail(None) => break Exit::Abandoned,
                Event::Mail(Some(Mail::Run(run))) => {
                    self.touch();
                    self.run_op(run).await;
                }
                Event::Mail(Some(Mail::Close {
                    report,
                    revoke,
                    done,
                })) => {
                    break Exit::Closed {
                        report,
                        revoke,
                        done: Some(done),
                    };
                }
                Event::Frame(Ok(Some(frame))) => {
                    self.touch();
                    self.frame(frame).await;
                }
                Event::Frame(Ok(None)) => break Exit::Eof,
                Event::Frame(Err(error)) if error.kind() == std::io::ErrorKind::InvalidData => {
                    tracing::warn!(thread = %self.key.thread, instance = %self.key.instance, %error,
                        "skipping an undecodable provider frame");
                }
                Event::Frame(Err(error)) => {
                    tracing::warn!(thread = %self.key.thread, %error, "provider output failed");
                    break Exit::Eof;
                }
                Event::Stderr(Ok(0)) | Event::Stderr(Err(_)) => self.stderr = None,
                Event::Stderr(Ok(read)) => self.keep_stderr(&chunk[..read]),
                Event::Idle => {
                    if self.idle_due().await {
                        break Exit::Idle;
                    }
                }
            }
        };
        self.finish(exit).await;
    }

    fn touch(&mut self) {
        self.deadline = Instant::now() + self.options.idle_timeout;
    }

    fn keep_stderr(&mut self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        tracing::debug!(thread = %self.key.thread, instance = %self.key.instance, stderr = %text);
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
            owner,
            events_to,
            steer,
            expect,
            handshake,
            op,
            done,
        } = run;
        self.handshake |= handshake;
        if let Some(owner) = owner {
            self.attempts.insert(owner.clone());
            self.owner = Some(owner);
        }
        if let Some(attempt) = &events_to {
            self.attempts.insert(attempt.clone());
        }
        let mut translation = match op(&mut self.protocol) {
            Ok(translation) => translation,
            Err(error) => {
                let _ = done.send(Err(SessionError::Protocol(error)));
                return;
            }
        };
        let mut ran = Ran {
            replies: vec![],
            completion: None,
        };
        match expect {
            Expect::Written => {}
            Expect::Replies => {
                for frame in &translation.outbound {
                    if let Some((id, operation)) = request(frame) {
                        let (sender, receiver) = oneshot::channel();
                        self.replies.push((id, operation, sender));
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
                        if let Some(previous) = self.completion.replace(sender) {
                            let _ = previous.send(Err("superseded by a newer operation".into()));
                        }
                    }
                }
                ran.completion = Some(receiver);
            }
        }
        if steer.is_some()
            && matches!(self.protocol, Protocol::Codex(_))
            && !translation.outbound.is_empty()
        {
            self.steers.push_back(steer);
        }
        let result = self.deliver(translation, events_to).await;
        let _ = done.send(result.map(|()| ran));
    }

    async fn frame(&mut self, frame: Value) {
        match self.protocol.receive(&frame) {
            Ok(translation) => {
                if let Err(error) = self.deliver(translation, None).await {
                    tracing::warn!(thread = %self.key.thread, %error, "provider frame was not fully handled");
                }
            }
            Err(error) => self.protocol_error(error).await,
        }
    }

    /// Commits each event, then writes outbound frames, then resolves replies:
    /// a native reply's facts are durable before the next outbound frame.
    async fn deliver(
        &mut self,
        translation: Translation,
        events_to: Option<RunAttemptId>,
    ) -> Result<(), SessionError> {
        let Translation {
            events,
            outbound,
            replies,
            completion,
            ..
        } = translation;
        if !events.is_empty() {
            match events_to.or_else(|| self.owner.clone()) {
                Some(attempt) => {
                    for event in events {
                        self.observe(&event);
                        self.provider(&attempt, event).await?;
                    }
                }
                None => {
                    tracing::debug!(thread = %self.key.thread, count = events.len(),
                        "dropping provider events of a session without a turn");
                }
            }
        }
        for frame in &outbound {
            write_frame(&mut self.input, frame)
                .await
                .map_err(|error| SessionError::Io(error.to_string()))?;
        }
        for reply in replies {
            if reply.operation == "turn/steer" {
                self.steers.pop_front();
            }
            if let Some(index) = self
                .replies
                .iter()
                .position(|(id, ..)| *id == reply.request)
            {
                let (.., waiter) = self.replies.remove(index);
                self.handshake = false;
                let _ = waiter.send(Ok(reply.result.0));
            }
        }
        if let Some(completion) = completion
            && let Some(waiter) = self.completion.take()
        {
            let _ = waiter.send(Ok(completion));
        }
        Ok(())
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

    async fn protocol_error(&mut self, error: ProtocolError) {
        match error {
            ProtocolError::Remote {
                operation,
                message,
                turn_completed,
            } => {
                if let Some(index) = self.replies.iter().position(|(_, op, _)| *op == operation) {
                    let (.., waiter) = self.replies.remove(index);
                    let _ = waiter.send(Err(message));
                    return;
                }
                if completion_operation(&operation)
                    && let Some(waiter) = self.completion.take()
                {
                    let _ = waiter.send(Err(message));
                    return;
                }
                let message_id = if operation == "turn/steer" {
                    self.steers.pop_front().flatten()
                } else {
                    None
                };
                let Some((operation, session_lost)) = failed_operation(&operation) else {
                    tracing::warn!(thread = %self.key.thread, %operation, %message,
                        "provider rejected an operation nobody waits for");
                    return;
                };
                let Some(attempt) = self.owner.clone() else {
                    return;
                };
                let failed = Input::Effect(EffectResult::ProviderFailed {
                    attempt,
                    operation,
                    message,
                    message_id,
                    turn_completed,
                    session_lost,
                });
                if let Err(error) = self.input(failed).await {
                    tracing::warn!(thread = %self.key.thread, %error, "could not record a provider failure");
                }
            }
            other => match self.completion.take() {
                Some(waiter) => {
                    let _ = waiter.send(Err(other.to_string()));
                }
                None => {
                    tracing::warn!(thread = %self.key.thread, error = %other, "invalid provider frame")
                }
            },
        }
    }

    async fn provider(
        &mut self,
        attempt: &RunAttemptId,
        event: ProviderEvent,
    ) -> Result<(), RuntimeError> {
        let mut retried = false;
        loop {
            let actor = self.registry.get_or_load(&self.key.thread).await?;
            match actor.provider(attempt.clone(), event.clone()).await {
                Err(RuntimeError::ActorStopped) if !retried => retried = true,
                result => return result.map(|_| ()),
            }
        }
    }

    async fn input(&mut self, input: Input) -> Result<(), RuntimeError> {
        let mut retried = false;
        loop {
            let actor = self.registry.get_or_load(&self.key.thread).await?;
            match actor.input(input.clone()).await {
                Err(RuntimeError::ActorStopped) if !retried => retried = true,
                result => return result.map(|_| ()),
            }
        }
    }

    /// Idle when the thread has no running turn or live request on this session;
    /// background work pins it up to `max_idle_pin`.
    async fn idle_due(&mut self) -> bool {
        if !self.mail.is_empty() {
            self.touch();
            return false;
        }
        let state = match self.registry.state(&self.key.thread).await {
            Ok(state) => state,
            Err(error) => {
                tracing::warn!(thread = %self.key.thread, %error, "cannot read the thread for idle release");
                self.touch();
                return false;
            }
        };
        if !self.mail.is_empty() {
            self.touch();
            return false;
        }
        match activity(&state, &self.key.instance, &self.attempts) {
            Activity::Busy => {
                self.pinned_since = None;
                self.touch();
                false
            }
            Activity::Pinned => {
                let now = Instant::now();
                let since = *self.pinned_since.get_or_insert(now);
                if now.duration_since(since) < self.options.max_idle_pin {
                    self.touch();
                    false
                } else {
                    tracing::warn!(thread = %self.key.thread, instance = %self.key.instance,
                        "releasing an idle provider session whose background work outlived the pin");
                    true
                }
            }
            Activity::Idle => true,
        }
    }

    async fn stop(&mut self) {
        self.input = Box::new(tokio::io::sink());
        if tokio::time::timeout(self.options.close_grace, self.control.wait())
            .await
            .is_err()
        {
            self.control.kill().await;
        }
    }

    async fn finish(mut self, exit: Exit) {
        let (report, error, revoke, done) = match exit {
            Exit::Closed {
                report,
                revoke,
                done,
            } => {
                self.stop().await;
                (report, None, revoke, done)
            }
            Exit::Idle => {
                self.stop().await;
                (true, None, false, None)
            }
            Exit::Abandoned => {
                self.stop().await;
                (false, None, false, None)
            }
            Exit::Eof => {
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
                    tracing::warn!(thread = %self.key.thread, instance = %self.key.instance,
                        stderr = %self.stderr_tail, "provider process exited unsuccessfully");
                }
                (
                    true,
                    (!success).then(|| "Provider process exited".to_owned()),
                    false,
                    None,
                )
            }
        };
        for (.., waiter) in self.replies.drain(..) {
            let _ = waiter.send(Err("The provider session closed.".into()));
        }
        if let Some(waiter) = self.completion.take() {
            let _ = waiter.send(Err("The provider session closed.".into()));
        }
        if report
            && !self.handshake
            && let Some(owner) = self.owner.clone()
            && let Err(error) = self
                .provider(&owner, ProviderEvent::SessionClosed { error })
                .await
        {
            tracing::warn!(thread = %self.key.thread, %error, "could not record a closed provider session");
        }
        if let Some(manager) = self.manager.upgrade() {
            manager.closed(&self.key, self.generation, revoke);
        }
        if let Some(done) = done {
            let _ = done.send(());
        }
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

/// The id and operation of an outbound request whose reply can be awaited.
fn request(frame: &Value) -> Option<(String, String)> {
    if frame["type"] == "control_request" {
        return Some((
            frame["request_id"].as_str()?.to_owned(),
            frame["request"]["subtype"].as_str()?.to_owned(),
        ));
    }
    let method = frame.get("method")?.as_str()?;
    Some((frame.get("id")?.to_string(), method.to_owned()))
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
        "turn/interrupt"
        | "interrupt"
        | "thread/backgroundTerminals/terminate"
        | "thread/backgroundTerminals/list" => (ProviderOperation::Interrupt, false),
        "thread/compact/start" => (ProviderOperation::Compact, false),
        "set_model" | "set_permission_mode" => (ProviderOperation::SetModel, false),
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Activity {
    Busy,
    Pinned,
    Idle,
}

pub(crate) fn activity(
    state: &State,
    instance: &str,
    attempts: &HashSet<RunAttemptId>,
) -> Activity {
    let running = state
        .active_run()
        .is_some_and(|run| run.selection.instance == instance);
    let asking = state.requests.iter().any(|request| {
        request.status == RequestStatus::Pending && attempts.contains(&request.attempt)
    });
    if running || asking {
        return Activity::Busy;
    }
    let background = state
        .background_work
        .values()
        .any(|work| attempts.contains(&work.attempt))
        || state
            .tasks
            .iter()
            .any(|task| !task.status.terminal() && attempts.contains(&task.attempt));
    if background {
        Activity::Pinned
    } else {
        Activity::Idle
    }
}
