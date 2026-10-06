//! In-memory provider processes and Host for session tests.
use super::super::*;
use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};
use tokio::io::ReadBuf;
use tokio::sync::{Notify, watch};

#[derive(Default)]
struct PipeState {
    chunks: VecDeque<Vec<u8>>,
    eof: bool,
    waker: Option<Waker>,
    pushed: u64,
    drained: u64,
}

/// The process's stdout. It records when the session asked for more after
/// reading everything pushed, i.e. finished handling every frame.
#[derive(Default)]
pub(crate) struct Pipe(Mutex<PipeState>);
impl Pipe {
    pub(crate) fn push(&self, frame: &Value) {
        let mut line = serde_json::to_vec(frame).unwrap();
        line.push(b'\n');
        let mut state = self.0.lock().unwrap();
        state.chunks.push_back(line);
        state.pushed += 1;
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }
    fn close(&self) {
        let mut state = self.0.lock().unwrap();
        state.eof = true;
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }
    pub(crate) fn drained(&self) -> bool {
        let state = self.0.lock().unwrap();
        state.eof || state.chunks.is_empty() && state.drained == state.pushed
    }
}
struct PipeReader(Arc<Pipe>);
impl tokio::io::AsyncRead for PipeReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut state = self.0.0.lock().unwrap();
        if let Some(chunk) = state.chunks.front_mut() {
            let n = buf.remaining().min(chunk.len());
            buf.put_slice(&chunk[..n]);
            chunk.drain(..n);
            if chunk.is_empty() {
                state.chunks.pop_front();
            }
            return Poll::Ready(Ok(()));
        }
        if state.eof {
            return Poll::Ready(Ok(()));
        }
        state.drained = state.pushed;
        state.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// Maps a written frame to the vocabulary the test compares, or drops it.
pub(crate) type Translate = fn(&Value) -> Option<Value>;
pub(crate) type Responder = Arc<dyn Fn(&Value) -> Vec<Value> + Send + Sync>;

pub(crate) type StdinClosed = Box<dyn FnOnce(Arc<FakeProcess>) + Send>;

/// One fake provider process, seen from the test.
pub(crate) struct FakeProcess {
    pub(crate) request: SpawnRequest,
    pub(crate) stdout: Arc<Pipe>,
    pub(crate) written: Mutex<Vec<Value>>,
    exit: watch::Sender<Option<bool>>,
    /// Runs instead of exiting when the session closes the process's stdin.
    pub(crate) on_stdin_close: Mutex<Option<StdinClosed>>,
    /// The Claude conversation messages the process saw, in order, as the CLI
    /// would record them in its transcript.
    pub(crate) messages: Mutex<Vec<Value>>,
}
impl FakeProcess {
    pub(crate) fn emit(&self, frame: Value) {
        self.record_message(&frame);
        self.stdout.push(&frame);
    }
    fn record_message(&self, frame: &Value) {
        if self.request.claude.is_some()
            && matches!(frame["type"].as_str(), Some("user" | "assistant"))
            && frame["parent_tool_use_id"].is_null()
            && let Some(uuid) = frame["uuid"].as_str()
        {
            let mut messages = self.messages.lock().unwrap();
            if !messages.iter().any(|message| message["uuid"] == uuid) {
                messages.push(frame.clone());
            }
        }
    }
    pub(crate) fn written(&self) -> Vec<Value> {
        self.written.lock().unwrap().clone()
    }
    /// Ends the process as the recorder's `runtime_exit` does.
    pub(crate) fn exit(&self, success: bool) {
        self.exit.send_if_modified(|status| {
            status.get_or_insert(success);
            true
        });
        self.stdout.close();
    }
    pub(crate) fn exited(&self) -> bool {
        self.exit.borrow().is_some()
    }
}

struct Stdin {
    process: Arc<FakeProcess>,
    host: Arc<FakeHost>,
    buffer: Vec<u8>,
}
impl tokio::io::AsyncWrite for Stdin {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let stall = *self.host.stall.lock().unwrap();
        if let Some(marker) = stall
            && bytes
                .windows(marker.len())
                .any(|window| window == marker.as_bytes())
        {
            return Poll::Pending;
        }
        self.buffer.extend_from_slice(bytes);
        while let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=end).collect();
            let frame: Value = serde_json::from_slice(&line).unwrap();
            self.process.record_message(&frame);
            self.process.written.lock().unwrap().push(frame.clone());
            self.host.written(&frame);
            let responder = self.host.responder.lock().unwrap().clone();
            for reply in responder(&frame) {
                self.process.stdout.push(&reply);
            }
        }
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
impl Drop for Stdin {
    /// A provider exits when its stdin closes.
    fn drop(&mut self) {
        let hook = self.process.on_stdin_close.lock().unwrap().take();
        match hook {
            Some(hook) => hook(self.process.clone()),
            None => self.process.exit(true),
        }
    }
}

struct Control(Arc<FakeProcess>, Arc<FakeHost>);
impl ProcessControl for Control {
    fn wait(&mut self) -> BoxFuture<'_, bool> {
        Box::pin(async move {
            let mut exit = self.0.exit.subscribe();
            let status = exit.wait_for(Option::is_some).await.map(|status| *status);
            status.ok().flatten().unwrap_or(false)
        })
    }
    fn kill(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.1
                .log(format!("kill:{}", self.0.request.target.key.thread));
            self.0.exit(false);
        })
    }
}

type SpawnHook = Arc<dyn Fn(&SpawnRequest) -> BoxFuture<'static, io::Result<()>> + Send + Sync>;
type SessionWriteHook = Arc<dyn Fn(&str) + Send + Sync>;

/// Records Host calls in order and hands out fake processes.
pub(crate) struct FakeHost {
    me: Weak<FakeHost>,
    pub(crate) processes: Mutex<Vec<Arc<FakeProcess>>>,
    pub(crate) log: Mutex<Vec<String>>,
    pub(crate) responder: Mutex<Responder>,
    pub(crate) before_spawn: Mutex<Option<SpawnHook>>,
    pub(crate) context: Mutex<WireContext>,
    pub(crate) claude: Mutex<ClaudeSettings>,
    pub(crate) prompts: Mutex<VecDeque<String>>,
    pub(crate) transcripts: Mutex<BTreeMap<String, String>>,
    pub(crate) outbound: Mutex<VecDeque<Value>>,
    pub(crate) translate: Mutex<Option<Translate>>,
    /// A write containing this text never completes, as a provider that stopped reading.
    pub(crate) stall: Mutex<Option<&'static str>>,
    pub(crate) before_session_write: Mutex<Option<SessionWriteHook>>,
    /// The managed Codex account's login parameters.
    pub(crate) codex_login: Mutex<Option<Value>>,
}
impl FakeHost {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            processes: Mutex::new(vec![]),
            log: Mutex::new(vec![]),
            responder: Mutex::new(Arc::new(|_| vec![])),
            before_spawn: Mutex::new(None),
            context: Mutex::new(WireContext {
                cwd: "/workspace".into(),
                client_name: "remote_agent_host".into(),
                client_version: "test".into(),
                ..WireContext::default()
            }),
            claude: Mutex::new(ClaudeSettings::default()),
            prompts: Mutex::new(VecDeque::new()),
            transcripts: Mutex::new(BTreeMap::new()),
            outbound: Mutex::new(VecDeque::new()),
            translate: Mutex::new(None),
            stall: Mutex::new(None),
            before_session_write: Mutex::new(None),
            codex_login: Mutex::new(None),
        })
    }
    pub(crate) fn respond(&self, responder: impl Fn(&Value) -> Vec<Value> + Send + Sync + 'static) {
        *self.responder.lock().unwrap() = Arc::new(responder);
    }
    pub(crate) fn before_spawn(
        &self,
        hook: impl Fn(&SpawnRequest) -> BoxFuture<'static, io::Result<()>> + Send + Sync + 'static,
    ) {
        *self.before_spawn.lock().unwrap() = Some(Arc::new(hook));
    }
    pub(crate) fn log(&self, entry: String) {
        self.log.lock().unwrap().push(entry);
    }
    pub(crate) fn logged(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
    pub(crate) fn process(&self, index: usize) -> Arc<FakeProcess> {
        self.processes.lock().unwrap()[index].clone()
    }
    pub(crate) fn spawned(&self) -> usize {
        self.processes.lock().unwrap().len()
    }
    pub(crate) fn last(&self) -> Option<Arc<FakeProcess>> {
        self.processes.lock().unwrap().last().cloned()
    }
    fn written(&self, frame: &Value) {
        let translate = *self.translate.lock().unwrap();
        let frame = match translate {
            Some(translate) => translate(frame),
            None => Some(frame.clone()),
        };
        if let Some(frame) = frame {
            self.outbound.lock().unwrap().push_back(frame);
        }
    }
}

impl SessionHost for FakeHost {
    fn spawn(&self, request: SpawnRequest) -> BoxFuture<'_, io::Result<ProviderProcess>> {
        Box::pin(async move {
            self.log(format!("spawn:{}", request.target.key.thread));
            let hook = self.before_spawn.lock().unwrap().clone();
            if let Some(hook) = hook {
                hook(&request).await?;
            }
            let process = Arc::new(FakeProcess {
                request,
                stdout: Arc::new(Pipe::default()),
                written: Mutex::new(vec![]),
                exit: watch::Sender::new(None),
                on_stdin_close: Mutex::new(None),
                messages: Mutex::new(vec![]),
            });
            self.processes.lock().unwrap().push(process.clone());
            Ok(ProviderProcess {
                input: Box::new(Stdin {
                    process: process.clone(),
                    host: self.me.upgrade().unwrap(),
                    buffer: vec![],
                }),
                output: Box::new(PipeReader(process.stdout.clone())),
                stderr: Box::new(tokio::io::empty()),
                control: Box::new(Control(process, self.me.upgrade().unwrap())),
            })
        })
    }
    fn codex_context(&self, target: LaunchTarget) -> BoxFuture<'_, Result<WireContext, String>> {
        Box::pin(async move {
            self.log(format!("context:{}", target.key.thread));
            Ok(self.context.lock().unwrap().clone())
        })
    }
    fn claude_settings(
        &self,
        target: LaunchTarget,
    ) -> BoxFuture<'_, Result<ClaudeSettings, String>> {
        Box::pin(async move {
            self.log(format!("context:{}", target.key.thread));
            Ok(self.claude.lock().unwrap().clone())
        })
    }
    fn images(
        &self,
        _: ThreadId,
        attachments: Vec<Attachment>,
    ) -> BoxFuture<'_, Result<Vec<PreparedImage>, String>> {
        Box::pin(async move {
            Ok(attachments
                .iter()
                .map(|file| PreparedImage {
                    attachment_id: file.id.clone(),
                    mime_type: file.mime_type.clone(),
                    base64: "aW1hZ2U=".into(),
                })
                .collect())
        })
    }
    /// A recorded transcript, or the one the CLI processes of the thread
    /// would have written.
    fn read_claude_session(
        &self,
        target: LaunchTarget,
        session: String,
    ) -> BoxFuture<'_, io::Result<String>> {
        Box::pin(async move {
            if let Some(transcript) = self.transcripts.lock().unwrap().get(&session) {
                return Ok(transcript.clone());
            }
            let messages: Vec<Value> = self
                .processes
                .lock()
                .unwrap()
                .iter()
                .filter(|process| process.request.target.key == target.key)
                .flat_map(|process| process.messages.lock().unwrap().clone())
                .collect();
            if messages.is_empty() {
                return Err(io::Error::new(io::ErrorKind::NotFound, session));
            }
            let mut parent = Value::Null;
            let mut transcript = String::new();
            for message in messages {
                let uuid = message["uuid"].clone();
                transcript.push_str(&json!({"type":message["type"],"uuid":uuid,"parentUuid":parent,"sessionId":session,"message":message["message"],"timestamp":"2026-10-06T00:00:00Z"}).to_string());
                transcript.push('\n');
                parent = uuid;
            }
            Ok(transcript)
        })
    }
    fn write_claude_session(
        &self,
        _: LaunchTarget,
        session: String,
        transcript: String,
    ) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            let hook = self.before_session_write.lock().unwrap().clone();
            if let Some(hook) = hook {
                hook(&session);
            }
            self.transcripts.lock().unwrap().insert(session, transcript);
            Ok(())
        })
    }
    fn revoke_credentials(&self, thread: &ThreadId, instance: Option<&str>) {
        self.log(format!("revoked:{thread}:{}", instance.unwrap_or("*")));
    }
    fn codex_account(&self, instance: String) -> BoxFuture<'_, Result<Option<Value>, String>> {
        Box::pin(async move {
            self.log(format!("account:{instance}"));
            Ok(self.codex_login.lock().unwrap().clone())
        })
    }
    fn refresh_codex_account(
        &self,
        instance: String,
        previous: Option<String>,
    ) -> BoxFuture<'_, Result<Value, String>> {
        Box::pin(async move {
            self.log(format!("refresh:{instance}:{previous:?}"));
            Ok(
                json!({"accessToken":"fresh","chatgptAccountId":"account-1","chatgptPlanType":"pro"}),
            )
        })
    }
    fn prompt_uuid(&self, effect_id: &str) -> String {
        self.prompts
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| derived_uuid("prompt", effect_id))
    }
}

/// A gate the test opens; waiters observe how many arrived.
#[derive(Default)]
pub(crate) struct Gate {
    pub(crate) arrived: Mutex<usize>,
    pub(crate) arrival: Notify,
    pub(crate) open: watch::Sender<bool>,
}
impl Gate {
    pub(crate) async fn pass(&self) {
        *self.arrived.lock().unwrap() += 1;
        self.arrival.notify_waiters();
        let mut open = self.open.subscribe();
        let _ = open.wait_for(|open| *open).await;
    }
    pub(crate) async fn until_arrived(&self, count: usize) {
        loop {
            let notified = self.arrival.notified();
            if *self.arrived.lock().unwrap() >= count {
                return;
            }
            notified.await;
        }
    }
    pub(crate) fn release(&self) {
        self.open.send_replace(true);
    }
}
