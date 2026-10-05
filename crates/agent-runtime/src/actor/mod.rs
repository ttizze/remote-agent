mod coalesce;
mod registry;

pub use coalesce::COALESCE_LIMIT;
pub use registry::*;

use crate::sync::{
    RESUME_MAX_REPLAY_FACTS, ResumeInput, ResumePlan, ThreadSnapshot, ThreadSubscribe,
    ThreadSubscription, ThreadUpdate, client_facts, decide_resume, replay_encoded_bytes,
    replay_raw_payload_safe,
};
use crate::{
    Clock, CommitBatch, RuntimeError, SNAPSHOT_INTERVAL, Settlement, ShellProjector, ShellRow,
    Store, StoreError, StoredFact, SystemClock, ThreadHead, ThreadShellProjector, attachment_paths,
    envelope_key, needs_recovery, search_changes,
};
use agent_domain::{
    Command, CommandId, EffectResult, FactBody, Input, InputEnvelope, ModelSelection,
    ProviderEvent, Reply, RunAttemptId, State, Step, ThreadId, ThreadMachine, Timestamp, apply,
};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone)]
pub struct ThreadView {
    pub state: Arc<State>,
    pub head: ThreadHead,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    pub reply: Reply,
    pub thread_seq: u64,
    pub global_seq: u64,
    pub replayed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOrigin {
    Client,
    Internal,
}

/// Commands that only the Host itself (sagas, sessions, executors) may send.
pub fn internal_command(command: &Command) -> bool {
    matches!(
        command,
        Command::NativeInput { .. }
            | Command::BindNativeChild { .. }
            | Command::AcceptFork { .. }
            | Command::AcceptDelegation { .. }
            | Command::AcceptTransfer { .. }
            | Command::TaskResult { .. }
            | Command::TaskProgress { .. }
            | Command::AcceptTaskWake { .. }
            | Command::ContinueRestart { .. }
            | Command::ReleasePrepared { .. }
            | Command::FailPrepared { .. }
    )
}

fn created_thread(command: &Command) -> Option<&ThreadId> {
    match command {
        Command::Create { thread, .. }
        | Command::AcceptFork { thread, .. }
        | Command::AcceptDelegation { thread, .. } => Some(thread),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandoffValue {
    pub model_window: Option<u64>,
    pub token_cap: u64,
}

/// The model catalog's handoff limits, queued to the actor when they differ from its state.
pub trait HandoffCatalog: Send + Sync {
    fn policy(&self, selection: &ModelSelection) -> Option<HandoffValue>;
}
pub struct NoHandoffCatalog;
impl HandoffCatalog for NoHandoffCatalog {
    fn policy(&self, _: &ModelSelection) -> Option<HandoffValue> {
        None
    }
}

/// Keeps an actor loaded while Host-owned work (such as a provider session) depends on it.
pub trait Residency: Send + Sync {
    fn pinned(&self, thread: &ThreadId) -> bool;
}
pub struct NoResidency;
impl Residency for NoResidency {
    fn pinned(&self, _: &ThreadId) -> bool {
        false
    }
}

#[derive(Clone)]
pub struct ActorContext {
    pub store: Store,
    pub clock: Arc<dyn Clock>,
    pub shell: Arc<dyn ShellProjector>,
    pub handoff: Arc<dyn HandoffCatalog>,
    pub residency: Arc<dyn Residency>,
    pub mailbox: usize,
}
impl ActorContext {
    pub fn new(store: Store) -> Self {
        Self {
            store,
            clock: Arc::new(SystemClock),
            shell: Arc::new(ThreadShellProjector),
            handoff: Arc::new(NoHandoffCatalog),
            residency: Arc::new(NoResidency),
            mailbox: 1024,
        }
    }
}

type Ack = oneshot::Sender<Result<Committed, RuntimeError>>;

pub(crate) enum Mail {
    Command {
        id: CommandId,
        command: Box<Command>,
        origin: CommandOrigin,
        reply: Ack,
    },
    Provider {
        attempt: RunAttemptId,
        event: Box<ProviderEvent>,
        ack: Option<Ack>,
    },
    EffectResult {
        effect_id: String,
        result: EffectResult,
        settlement: Settlement,
        ack: Ack,
    },
    Input {
        input: Input,
        ack: Ack,
    },
    Timer,
    Subscribe {
        options: ThreadSubscribe,
        reply: oneshot::Sender<Result<ThreadSubscription, RuntimeError>>,
    },
    View {
        reply: oneshot::Sender<ThreadView>,
    },
    Evict {
        idle_for: Duration,
        reply: oneshot::Sender<bool>,
    },
}

/// The only writer for one thread. Cloned handles share the same mailbox.
#[derive(Clone)]
pub struct ActorHandle {
    thread: ThreadId,
    mail: mpsc::Sender<Mail>,
}

impl ActorHandle {
    /// Loads the thread (snapshot plus later facts) and starts its actor.
    pub async fn spawn(context: ActorContext, thread: ThreadId) -> Result<Self, RuntimeError> {
        let (sender, receiver) = mpsc::channel(context.mailbox.max(1));
        let actor = Actor::load(context, thread.clone(), receiver).await?;
        tokio::spawn(actor.run());
        Ok(Self {
            thread,
            mail: sender,
        })
    }

    pub fn thread(&self) -> &ThreadId {
        &self.thread
    }

    pub fn is_closed(&self) -> bool {
        self.mail.is_closed()
    }

    pub async fn dispatch(
        &self,
        id: CommandId,
        command: Command,
        origin: CommandOrigin,
    ) -> Result<Committed, RuntimeError> {
        self.request(|reply| Mail::Command {
            id,
            command: Box::new(command),
            origin,
            reply,
        })
        .await?
    }

    /// Provider input whose caller waits for the commit (e.g. before the next outbound frame).
    pub async fn provider(
        &self,
        attempt: RunAttemptId,
        event: ProviderEvent,
    ) -> Result<Committed, RuntimeError> {
        self.request(|ack| Mail::Provider {
            attempt,
            event: Box::new(event),
            ack: Some(ack),
        })
        .await?
    }

    /// Provider input that only waits for mailbox space.
    pub async fn provider_nowait(
        &self,
        attempt: RunAttemptId,
        event: ProviderEvent,
    ) -> Result<(), RuntimeError> {
        self.send(Mail::Provider {
            attempt,
            event: Box::new(event),
            ack: None,
        })
        .await
    }

    /// Feeds an effect result back and marks its outbox row succeeded in the same commit.
    pub async fn effect_result(
        &self,
        effect_id: String,
        result: EffectResult,
    ) -> Result<Committed, RuntimeError> {
        self.settle_effect(effect_id, result, Settlement::Succeeded)
            .await
    }

    /// Feeds an effect result back and settles its outbox row as `settlement` in the same commit.
    pub async fn settle_effect(
        &self,
        effect_id: String,
        result: EffectResult,
        settlement: Settlement,
    ) -> Result<Committed, RuntimeError> {
        self.request(|ack| Mail::EffectResult {
            effect_id,
            result,
            settlement,
            ack,
        })
        .await?
    }

    /// Host inputs such as `RuntimeOpened`, `CheckpointScope`, `NativeSessionReset` and `Recover`.
    pub async fn input(&self, input: Input) -> Result<Committed, RuntimeError> {
        self.request(|ack| Mail::Input { input, ack }).await?
    }

    pub async fn timer(&self) -> Result<(), RuntimeError> {
        self.send(Mail::Timer).await
    }

    pub async fn subscribe(
        &self,
        options: ThreadSubscribe,
    ) -> Result<ThreadSubscription, RuntimeError> {
        self.request(|reply| Mail::Subscribe { options, reply })
            .await?
    }

    pub async fn view(&self) -> Result<ThreadView, RuntimeError> {
        self.request(|reply| Mail::View { reply }).await
    }

    /// Stops the actor if it is idle; the caller must hold the registry's key lock.
    pub(crate) async fn evict(&self, idle_for: Duration) -> bool {
        let stopped = self
            .request(|reply| Mail::Evict { idle_for, reply })
            .await
            .unwrap_or(true);
        if stopped {
            self.mail.closed().await;
        }
        stopped
    }

    pub(crate) async fn send(&self, mail: Mail) -> Result<(), RuntimeError> {
        self.mail
            .send(mail)
            .await
            .map_err(|_| RuntimeError::ActorStopped)
    }

    async fn request<T>(
        &self,
        mail: impl FnOnce(oneshot::Sender<T>) -> Mail,
    ) -> Result<T, RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.send(mail(reply)).await?;
        response.await.map_err(|_| RuntimeError::ActorStopped)
    }
}

pub(crate) struct Actor {
    context: ActorContext,
    thread: ThreadId,
    state: Arc<State>,
    head: ThreadHead,
    last_at: Option<Timestamp>,
    shell: Option<ShellRow>,
    snapshot_due: bool,
    subscribers: Vec<mpsc::Sender<ThreadUpdate>>,
    mail: mpsc::Receiver<Mail>,
    held: VecDeque<Mail>,
    own: VecDeque<Input>,
    active_at: Instant,
    broken: bool,
}

impl Actor {
    pub(crate) async fn load(
        context: ActorContext,
        thread: ThreadId,
        mail: mpsc::Receiver<Mail>,
    ) -> Result<Self, RuntimeError> {
        let id = thread.clone();
        let (loaded, shell) = context
            .store
            .blocking(move |store| Ok((store.load_thread(&id)?, store.shell(&id)?)))
            .await?;
        Ok(Self {
            context,
            thread,
            state: Arc::new(loaded.state),
            head: loaded.head,
            last_at: loaded.last_at,
            shell: shell.map(|(_, row)| row),
            snapshot_due: loaded.snapshot_stale,
            subscribers: Vec::new(),
            mail,
            held: VecDeque::new(),
            own: VecDeque::new(),
            active_at: Instant::now(),
            broken: false,
        })
    }

    pub(crate) async fn run(mut self) {
        while !self.broken {
            if let Some(input) = self.own.pop_front() {
                if let Err(error) = self.step(input, None).await {
                    tracing::warn!(thread = %self.thread, %error, "actor self input failed");
                }
                continue;
            }
            let mail = match self.held.pop_front() {
                Some(mail) => mail,
                None => {
                    let wake = self.wake_in();
                    tokio::select! {
                        mail = self.mail.recv() => match mail {
                            Some(mail) => mail,
                            None => return,
                        },
                        () = tokio::time::sleep(wake.unwrap_or_default()), if wake.is_some() => Mail::Timer,
                    }
                }
            };
            if !self.handle(mail).await {
                return;
            }
        }
        tracing::error!(thread = %self.thread, "actor stopped after its state could not be reloaded");
    }

    async fn handle(&mut self, mail: Mail) -> bool {
        if !matches!(mail, Mail::Evict { .. } | Mail::View { .. }) {
            self.active_at = Instant::now();
        }
        match mail {
            Mail::Command {
                id,
                command,
                origin,
                reply,
            } => {
                let result = self.command(id, command, origin).await;
                let _ = reply.send(result);
            }
            Mail::Provider {
                attempt,
                event,
                ack,
            } => {
                let (attempt, event, acks) = self.coalesce(attempt, event, ack);
                let result = self.step(Input::Provider { attempt, event }, None).await;
                for ack in acks {
                    let _ = ack.send(result.clone());
                }
            }
            Mail::EffectResult {
                effect_id,
                result,
                settlement,
                ack,
            } => {
                let result = self
                    .step(Input::Effect(result), Some((effect_id, settlement)))
                    .await;
                let _ = ack.send(result);
            }
            Mail::Input { input, ack } => {
                let result = match input {
                    Input::Command { .. } => Err(RuntimeError::InvalidInput("Input::Command")),
                    input => self.step(input, None).await,
                };
                let _ = ack.send(result);
            }
            Mail::Timer => {
                if let Err(error) = self.step(Input::Timer, None).await {
                    tracing::warn!(thread = %self.thread, %error, "timer step failed");
                }
            }
            Mail::Subscribe { options, reply } => {
                let result = self.subscribe(options).await;
                let _ = reply.send(result);
            }
            Mail::View { reply } => {
                let _ = reply.send(self.view());
            }
            Mail::Evict { idle_for, reply } => {
                let idle = self.idle(idle_for).await;
                let _ = reply.send(idle);
                return !idle;
            }
        }
        true
    }

    fn coalesce(
        &mut self,
        attempt: RunAttemptId,
        mut event: Box<ProviderEvent>,
        ack: Option<Ack>,
    ) -> (RunAttemptId, Box<ProviderEvent>, Vec<Ack>) {
        let mut acks: Vec<Ack> = ack.into_iter().collect();
        if !coalesce::is_delta(&event) {
            return (attempt, event, acks);
        }
        loop {
            let next = match self.held.pop_front() {
                Some(next) => next,
                None => match self.mail.try_recv() {
                    Ok(next) => next,
                    Err(_) => break,
                },
            };
            match next {
                Mail::Provider {
                    attempt: next_attempt,
                    event: next_event,
                    ack: next_ack,
                } if coalesce::join(&attempt, &mut event, &next_attempt, &next_event) => {
                    acks.extend(next_ack);
                }
                other => {
                    self.held.push_front(other);
                    break;
                }
            }
        }
        (attempt, event, acks)
    }

    async fn command(
        &mut self,
        id: CommandId,
        command: Box<Command>,
        origin: CommandOrigin,
    ) -> Result<Committed, RuntimeError> {
        if origin == CommandOrigin::Client && internal_command(&command) {
            return Ok(self.unpersisted(rejected("internal-command")));
        }
        if created_thread(&command).is_some_and(|thread| thread != &self.thread) {
            return Ok(self.unpersisted(rejected("thread-mismatch")));
        }
        let lookup = id.clone();
        let stored = self
            .context
            .store
            .blocking(move |store| store.receipt(&lookup))
            .await?;
        if stored
            .as_ref()
            .is_some_and(|stored| stored.thread != self.thread)
        {
            return Ok(self.unpersisted(rejected("command-id-conflict")));
        }
        let (at, step) = self.decide(Input::Command {
            id,
            command,
            receipt: stored.as_ref().map(|stored| stored.receipt.clone()),
        });
        if let Some(stored) = stored
            && step.facts.is_empty()
            && step.receipt.as_ref() == Some(&stored.receipt)
        {
            return Ok(Committed {
                reply: step.reply,
                thread_seq: stored.thread_seq,
                global_seq: stored.global_seq,
                replayed: true,
            });
        }
        self.persist(at, step, None).await
    }

    async fn step(
        &mut self,
        input: Input,
        settle: Option<(String, Settlement)>,
    ) -> Result<Committed, RuntimeError> {
        let (at, step) = self.decide(input);
        self.persist(at, step, settle).await
    }

    fn decide(&self, input: Input) -> (Timestamp, Step) {
        let now = self.context.clock.now();
        let at = match &self.last_at {
            Some(last) if *last > now => last.clone(),
            _ => now,
        };
        let envelope = InputEnvelope {
            at: at.clone(),
            key: envelope_key(&self.thread, self.head.input_seq + 1),
            input,
        };
        (at, ThreadMachine::step(&self.state, &envelope))
    }

    /// Folds, commits, publishes and only then replies. A failed commit reloads the
    /// committed state, which is safe because `step` is pure.
    async fn persist(
        &mut self,
        at: Timestamp,
        step: Step,
        settle: Option<(String, Settlement)>,
    ) -> Result<Committed, RuntimeError> {
        let Step {
            facts,
            effects,
            reply,
            receipt,
        } = step;
        if facts.is_empty() && effects.is_empty() && receipt.is_none() && settle.is_none() {
            return Ok(self.unpersisted(reply));
        }
        let folded = {
            let state = Arc::make_mut(&mut self.state);
            facts.iter().try_for_each(|fact| apply(state, fact))
        };
        if let Err(error) = folded {
            self.reload().await;
            return Err(error.into());
        }
        let base = self.head;
        let thread_seq = base.thread_seq + facts.len() as u64;
        let snapshot = if self.snapshot_due
            || base.thread_seq / SNAPSHOT_INTERVAL != thread_seq / SNAPSHOT_INTERVAL
        {
            match serde_json::to_vec(&*self.state) {
                Ok(blob) => Some(blob),
                Err(error) => {
                    self.reload().await;
                    return Err(StoreError::from(error).into());
                }
            }
        } else {
            None
        };
        let shell = self.context.shell.project(&self.state);
        let wrote_snapshot = snapshot.is_some();
        let batch = CommitBatch {
            thread: self.thread.clone(),
            at: at.clone(),
            base_thread_seq: base.thread_seq,
            input_seq: base.input_seq + 1,
            receipt,
            search: search_changes(&self.state, &facts),
            attachment_paths: attachment_paths(&facts),
            facts,
            effects,
            settle,
            shell: shell
                .clone()
                .filter(|shell| Some(shell) != self.shell.as_ref()),
            needs_recovery: needs_recovery(&self.state),
            snapshot,
        };
        match self.context.store.commit(batch).await {
            Ok(outcome) => {
                self.head = outcome.head;
                self.last_at = Some(at);
                if shell.is_some() {
                    self.shell = shell;
                }
                if wrote_snapshot {
                    self.snapshot_due = false;
                }
                if !outcome.facts.is_empty() {
                    self.publish(ThreadUpdate::Facts(client_facts(&outcome.facts)));
                    self.queue_handoff(&outcome.facts);
                }
                Ok(Committed {
                    reply,
                    thread_seq: self.head.thread_seq,
                    global_seq: self.head.global_seq,
                    replayed: false,
                })
            }
            Err(StoreError::CommandConflict(_)) => {
                self.reload().await;
                Ok(self.unpersisted(rejected("command-id-conflict")))
            }
            Err(error) => {
                tracing::warn!(thread = %self.thread, %error, "discarding an uncommitted step");
                self.reload().await;
                Err(error.into())
            }
        }
    }

    async fn reload(&mut self) {
        let thread = self.thread.clone();
        match self
            .context
            .store
            .blocking(move |store| store.load_thread(&thread))
            .await
        {
            Ok(loaded) => {
                self.state = Arc::new(loaded.state);
                self.head = loaded.head;
                self.last_at = loaded.last_at.max(self.last_at.take());
                self.snapshot_due |= loaded.snapshot_stale;
                self.own.clear();
            }
            Err(error) => {
                tracing::error!(thread = %self.thread, %error, "cannot reload the thread");
                self.broken = true;
            }
        }
    }

    fn unpersisted(&self, reply: Reply) -> Committed {
        Committed {
            reply,
            thread_seq: self.head.thread_seq,
            global_seq: self.head.global_seq,
            replayed: false,
        }
    }

    fn publish(&mut self, update: ThreadUpdate) {
        self.subscribers
            .retain(|subscriber| subscriber.try_send(update.clone()).is_ok());
    }

    fn queue_handoff(&mut self, facts: &[crate::StoredFact]) {
        if !facts.iter().any(|stored| {
            matches!(
                stored.fact.body,
                FactBody::ThreadCreated { .. } | FactBody::ModelSelected { .. }
            )
        }) {
            return;
        }
        let Some(thread) = &self.state.thread else {
            return;
        };
        let Some(policy) = self.context.handoff.policy(&thread.selection) else {
            return;
        };
        let instance = thread.selection.instance.clone();
        if self.state.context_windows.get(&instance).copied() != policy.model_window
            || self.state.handoff_token_cap != Some(policy.token_cap)
        {
            self.own.push_back(Input::HandoffPolicy {
                instance,
                model_window: policy.model_window,
                token_cap: policy.token_cap,
            });
        }
    }

    fn view(&self) -> ThreadView {
        ThreadView {
            state: self.state.clone(),
            head: self.head,
        }
    }

    /// Registration happens between steps, so no committed fact is lost or sent twice.
    async fn subscribe(
        &mut self,
        options: ThreadSubscribe,
    ) -> Result<ThreadSubscription, RuntimeError> {
        let (sender, updates) = mpsc::channel(options.capacity.max(4));
        let replay = match options.after_global_seq {
            Some(after) => self.replay(after).await?,
            None => None,
        };
        let first = match replay {
            Some(facts) => (!facts.is_empty()).then_some(ThreadUpdate::Facts(facts)),
            None => Some(ThreadUpdate::Snapshot(ThreadSnapshot::build(
                &self.state,
                self.head,
                options.accept_bounded_snapshot,
            ))),
        };
        let marker = options
            .request_completion_marker
            .then_some(ThreadUpdate::Synchronized);
        for update in first.into_iter().chain(marker) {
            sender
                .try_send(update)
                .expect("a new subscription has room for its first updates");
        }
        self.subscribers.push(sender);
        Ok(ThreadSubscription { updates })
    }

    /// The facts after `after`, or `None` when a snapshot should replace them.
    async fn replay(&self, after: u64) -> Result<Option<Arc<[StoredFact]>>, RuntimeError> {
        let high_water = self.head.global_seq;
        if after > high_water {
            return Ok(None);
        }
        let thread = self.thread.clone();
        let gap = self
            .context
            .store
            .blocking(move |store| store.fact_gap(&thread, after))
            .await?;
        // A recreated thread replaces its replay with a snapshot unless it is deleted.
        let live = self
            .state
            .thread
            .as_ref()
            .is_some_and(|thread| thread.deleted_at.is_none());
        if gap.facts > RESUME_MAX_REPLAY_FACTS
            || !replay_raw_payload_safe(gap.bytes)
            || gap.contains_created && live
        {
            return Ok(None);
        }
        let thread = self.thread.clone();
        let facts: Arc<[StoredFact]> = self
            .context
            .store
            .blocking(move |store| store.facts_after(Some(&thread), after))
            .await?
            .into();
        let facts = client_facts(&facts);
        let plan = decide_resume(ResumeInput {
            after,
            high_water,
            replay_facts: facts.len() as u64,
            replay_encoded_bytes: replay_encoded_bytes(&facts),
        });
        Ok(matches!(plan, ResumePlan::Replay { .. }).then_some(facts))
    }

    /// The single timer: the delay until `snoozed_until`, if any.
    fn wake_in(&self) -> Option<Duration> {
        let until = self.state.thread.as_ref()?.snoozed_until.as_ref()?;
        let delay = until.millis() - self.context.clock.now().millis();
        Some(Duration::from_millis(delay.max(0) as u64))
    }

    async fn idle(&mut self, idle_for: Duration) -> bool {
        self.subscribers
            .retain(|subscriber| !subscriber.is_closed());
        if !self.subscribers.is_empty()
            || !self.held.is_empty()
            || !self.own.is_empty()
            || !self.mail.is_empty()
            || self.wake_in().is_some()
            || self.active_at.elapsed() < idle_for
            || self.context.residency.pinned(&self.thread)
        {
            return false;
        }
        let thread = self.thread.clone();
        matches!(
            self.context
                .store
                .blocking(move |store| store.has_open_effects(&thread))
                .await,
            Ok(false)
        )
    }
}

fn rejected(reason: &str) -> Reply {
    Reply::Rejected {
        reason: reason.into(),
    }
}

#[cfg(test)]
pub(crate) mod tests;
