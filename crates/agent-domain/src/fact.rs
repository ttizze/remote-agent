use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fact {
    pub at: Timestamp,
    pub body: FactBody,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FactBody {
    TaskNamed {
        id: NodeId,
        title: String,
    },
    ForkPrepared {
        command: CommandId,
        target: ThreadId,
        child_command: Box<Command>,
        instance: String,
        head: Option<String>,
    },
    ForkResolved {
        command: CommandId,
    },
    NativeSessionInherited {
        instance: String,
        native_thread: String,
        head: Option<String>,
    },
    AttemptHeadRecorded {
        attempt: RunAttemptId,
        head: Option<String>,
    },
    BackgroundTaskStarted {
        key: String,
        tool: String,
        description: String,
        kind: BackgroundKind,
        attempt: RunAttemptId,
    },
    BackgroundTaskFinished {
        key: String,
        summary: Option<String>,
    },
    BackgroundWorkStopped,
    WakeReportsConsumed,
    PromptOffered {
        attempt: RunAttemptId,
        key: String,
    },
    PromptEchoModeLearned {
        mode: PromptEchoMode,
    },
    PromptFrameObserved,
    PromptConfirmed,
    PromptOutputHeld {
        events: Vec<ProviderEvent>,
    },
    PromptOutputReleased,
    NativeContinuationQueued {
        run: RunId,
        events: Vec<ProviderEvent>,
    },
    NativeContinuationReleased {
        run: RunId,
    },
    ThreadCreated {
        id: ThreadId,
        project: String,
        title: String,
        selection: ModelSelection,
        runtime_mode: RuntimeMode,
        interaction_mode: InteractionMode,
    },
    ChildEventDeferred {
        key: String,
        event: Box<ProviderEvent>,
    },
    ChildEventsReleased {
        key: String,
    },
    NativeChildBound {
        native_thread: Option<String>,
        owner: RunAttemptId,
        parent: ThreadId,
        task: NodeId,
    },
    NativeChildTurnBound {
        native_turn: Option<String>,
    },
    ThreadRenamed {
        title: String,
    },
    ThreadArchived {
        archived: bool,
    },
    StopRequested {
        attempt: RunAttemptId,
    },
    ThreadDeleted,
    ThreadSettled {
        settled: bool,
        at: Timestamp,
    },
    ThreadSnoozed {
        until: Option<Timestamp>,
    },
    ThreadPinned {
        pinned: bool,
        order: Option<String>,
    },
    ThreadActiveReordered {
        order: String,
    },
    ThreadVisited {
        at: Timestamp,
    },
    AutoSettleChanged {
        enabled: bool,
    },
    ModelSelected {
        selection: ModelSelection,
    },
    RuntimeModeChanged {
        mode: RuntimeMode,
    },
    InteractionModeChanged {
        mode: InteractionMode,
    },
    MessageCreated {
        id: MessageId,
        run: Option<RunId>,
        role: Role,
        text: String,
        attachments: Vec<Attachment>,
        intent: InputIntent,
        created_by: MessageAuthor,
        creation_source: String,
    },
    MessageEdited {
        id: MessageId,
        text: String,
        attachments: Option<Vec<Attachment>>,
    },
    MessageAdopted {
        id: MessageId,
        run: RunId,
        intent: InputIntent,
    },
    MessageFinished {
        id: MessageId,
    },
    RunRequested {
        id: RunId,
        message: MessageId,
        ordinal: u64,
        selection: ModelSelection,
        status: RunStatus,
        queue_position: Option<u64>,
        held: bool,
        source_plan: Option<PlanId>,
    },
    RunStarted {
        id: RunId,
    },
    RunPrepared {
        id: RunId,
    },
    QueueHeld {
        id: RunId,
        held: bool,
    },
    QueueReordered {
        order: Vec<RunId>,
    },
    RunWaitingForCapture {
        id: RunId,
        terminal: RunStatus,
    },
    RunFinished {
        id: RunId,
        status: RunStatus,
    },
    AttemptStarted {
        id: RunAttemptId,
        run: RunId,
        ordinal: u64,
    },
    AttemptFinished {
        id: RunAttemptId,
        status: AttemptStatus,
    },
    SessionBound {
        attempt: RunAttemptId,
        native_thread: String,
    },
    TurnBound {
        attempt: RunAttemptId,
        native_turn: Option<String>,
    },
    NativeHeadChanged {
        instance: String,
        head: Option<String>,
    },
    UsageRecorded {
        attempt: RunAttemptId,
        usage: TokenUsage,
    },
    ItemStarted {
        id: TurnItemId,
        run: Option<RunId>,
        attempt: Option<RunAttemptId>,
        native_key: String,
        ordinal: u64,
        kind: ItemKind,
    },
    ItemTextAppended {
        id: TurnItemId,
        offset: usize,
        text: String,
    },
    ItemTextReplaced {
        id: TurnItemId,
        text: String,
    },
    ItemDetailChanged {
        id: TurnItemId,
        kind: ItemKind,
    },
    ItemCompleted {
        id: TurnItemId,
        status: ItemStatus,
    },
    RequestOpened {
        owner_path: Vec<String>,
        id: RuntimeRequestId,
        attempt: RunAttemptId,
        native_key: String,
        body: RequestBody,
        capability: ResponseCapability,
    },
    RequestResolved {
        id: RuntimeRequestId,
        status: RequestStatus,
        decision: Option<ApprovalDecision>,
        answers: Option<Answers>,
        attachments: BTreeMap<String, Vec<Attachment>>,
    },
    PlanStarted {
        id: PlanId,
        run: RunId,
        native_key: String,
        kind: PlanKind,
    },
    PlanMarkdownAppended {
        id: PlanId,
        offset: usize,
        text: String,
    },
    PlanMarkdownReplaced {
        id: PlanId,
        text: String,
    },
    PlanStepsReplaced {
        id: PlanId,
        steps: Vec<PlanStep>,
    },
    PlanImplemented {
        id: PlanId,
        run: RunId,
    },
    CheckpointCaptured {
        id: CheckpointId,
        run: Option<RunId>,
        run_ordinal: u64,
        native_heads: BTreeMap<String, Option<String>>,
        file_ref: String,
    },
    RollbackRequested {
        command: CommandId,
        checkpoint: CheckpointId,
        restore_files: bool,
    },
    RolledBack {
        command: CommandId,
        checkpoint: CheckpointId,
    },
    RollbackFailed {
        command: CommandId,
        message: String,
    },
    ForkAccepted {
        parent: ThreadId,
        boundary: u64,
        history: Vec<Item>,
    },
    TransferOpened {
        id: ContextTransferId,
        kind: TransferKind,
        source: ThreadId,
        target: ThreadId,
        boundary: u64,
        text: String,
    },
    TransferConsumed {
        id: ContextTransferId,
        run: RunId,
    },
    TaskStarted {
        id: NodeId,
        native_key: String,
        run: Option<RunId>,
        attempt: RunAttemptId,
        child: ThreadId,
        parent: Option<NodeId>,
        app_owned: bool,
        prompt: String,
        model: Option<String>,
        wake: CompletionWake,
    },
    TaskProgressed {
        id: NodeId,
        progress: String,
        model: Option<String>,
    },
    TaskFinished {
        id: NodeId,
        status: ItemStatus,
        result: String,
    },
    TaskReopened {
        id: NodeId,
    },
    TaskWakeChanged {
        id: NodeId,
        wake: CompletionWake,
    },
    TaskDeliveryChanged {
        id: NodeId,
        state: DeliveryState,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FoldError {
    #[error("fact references missing {0}")]
    Missing(&'static str),
    #[error("text offset does not match the projection")]
    Offset,
    #[error("fact conflicts with an existing identity")]
    Conflict,
}
fn find_mut<'a, T>(
    items: &'a mut [T],
    name: &'static str,
    matches: impl Fn(&T) -> bool,
) -> Result<&'a mut T, FoldError> {
    items
        .iter_mut()
        .find(|i| matches(i))
        .ok_or(FoldError::Missing(name))
}
fn message_for_item<'a>(state: &'a mut State, id: &TurnItemId) -> Option<&'a mut Message> {
    let item = state.items.iter().find(|i| &i.id == id)?;
    let message = match &item.kind {
        ItemKind::AssistantMessage { message } => message.clone(),
        _ => return None,
    };
    state.messages.iter_mut().find(|m| m.id == message)
}
/// Apply one committed fact. I/O owners call this same fold on Host and clients.
pub fn apply(state: &mut State, fact: &Fact) -> Result<(), FoldError> {
    let at = &fact.at;
    use FactBody::*;
    match &fact.body {
        TaskNamed { id, title } => {
            find_mut(&mut state.tasks, "task", |t| &t.id == id)?.title = Some(title.clone())
        }
        ForkPrepared {
            command,
            target,
            child_command,
            instance,
            head,
        } => {
            state.pending_forks.insert(
                command.clone(),
                PendingFork {
                    target: target.clone(),
                    child_command: child_command.clone(),
                    instance: instance.clone(),
                    head: head.clone(),
                },
            );
        }
        ForkResolved { command } => {
            state.pending_forks.remove(command);
        }
        NativeSessionInherited {
            instance,
            native_thread,
            head,
        } => {
            state
                .native_sessions
                .insert(instance.clone(), native_thread.clone());
            state.native_heads.insert(instance.clone(), head.clone());
        }
        AttemptHeadRecorded { attempt, head } => {
            find_mut(&mut state.attempts, "attempt", |a| &a.id == attempt)?.native_head =
                head.clone();
        }
        BackgroundTaskStarted {
            key,
            tool,
            description,
            kind,
            attempt,
        } => {
            state.background_work.insert(
                key.clone(),
                BackgroundWork {
                    key: key.clone(),
                    tool: tool.clone(),
                    description: description.clone(),
                    kind: *kind,
                    attempt: attempt.clone(),
                },
            );
        }
        BackgroundTaskFinished { key, summary } => {
            state.background_work.remove(key);
            if let Some(summary) = summary {
                state.wake_reports.insert(key.clone(), summary.clone());
            }
        }
        BackgroundWorkStopped => {
            state.background_work.clear();
            state.wake_reports.clear();
        }
        WakeReportsConsumed => state.wake_reports.clear(),
        PromptOffered { attempt, key } => {
            state.pending_prompt = Some(PendingPrompt {
                attempt: attempt.clone(),
                key: key.clone(),
                confirmed: false,
                frames_before_echo: 0,
                held: vec![],
            })
        }
        PromptEchoModeLearned { mode } => state.prompt_echo_mode = *mode,
        PromptFrameObserved => {
            if let Some(prompt) = &mut state.pending_prompt {
                prompt.frames_before_echo += 1;
            }
        }
        PromptConfirmed => {
            if let Some(prompt) = &mut state.pending_prompt {
                prompt.confirmed = true;
            }
        }
        PromptOutputHeld { events } => {
            if let Some(prompt) = &mut state.pending_prompt {
                prompt.held.extend(events.clone());
            }
        }
        PromptOutputReleased => {
            if let Some(prompt) = &mut state.pending_prompt {
                prompt.held.clear();
                prompt.frames_before_echo = 0;
            }
        }
        NativeContinuationQueued { run, events } => {
            find_mut(&mut state.runs, "run", |r| &r.id == run)?.continuation = true;
            state
                .native_continuations
                .insert(run.clone(), events.clone());
        }
        NativeContinuationReleased { run } => {
            state.native_continuations.remove(run);
        }
        ThreadCreated {
            id,
            project,
            title,
            selection,
            runtime_mode,
            interaction_mode,
        } => {
            if state.thread.is_some() {
                return Err(FoldError::Conflict);
            }
            state.thread = Some(Thread {
                id: id.clone(),
                project: project.clone(),
                title: title.clone(),
                selection: selection.clone(),
                runtime_mode: *runtime_mode,
                interaction_mode: *interaction_mode,
                created_at: at.clone(),
                updated_at: at.clone(),
                archived_at: None,
                deleted_at: None,
                settled: None,
                settled_at: None,
                snoozed_until: None,
                pinned_at: None,
                pin_order: None,
                active_order: None,
                last_visited_at: None,
                auto_settle: true,
                parent: None,
                fork_boundary: None,
            });
        }
        ChildEventDeferred { key, event } => state
            .pending_children
            .entry(key.clone())
            .or_default()
            .push(*event.clone()),
        ChildEventsReleased { key } => {
            state.pending_children.remove(key);
        }
        NativeChildBound {
            native_thread,
            owner,
            parent,
            task,
        } => {
            state.native_owner = Some(owner.clone());
            state.native_child_thread = native_thread.clone();
            state.native_child_turn = None;
            state.stopping.remove(owner);
            state.native_parent = Some((parent.clone(), task.clone()));
            state
                .thread
                .as_mut()
                .ok_or(FoldError::Missing("thread"))?
                .parent = Some(parent.clone());
        }
        NativeChildTurnBound { native_turn } => {
            state.native_child_turn = native_turn.clone();
        }
        ThreadRenamed { title } => {
            state
                .thread
                .as_mut()
                .ok_or(FoldError::Missing("thread"))?
                .title = title.clone()
        }
        ThreadArchived { archived } => {
            state
                .thread
                .as_mut()
                .ok_or(FoldError::Missing("thread"))?
                .archived_at = archived.then(|| at.clone())
        }
        StopRequested { attempt } => {
            state.stopping.insert(attempt.clone());
        }
        ThreadDeleted => {
            state
                .thread
                .as_mut()
                .ok_or(FoldError::Missing("thread"))?
                .deleted_at = Some(at.clone())
        }
        ThreadSettled { settled, at } => {
            let t = state.thread.as_mut().ok_or(FoldError::Missing("thread"))?;
            t.settled = Some(*settled);
            t.settled_at = settled.then(|| at.clone());
            if *settled {
                t.pinned_at = None;
                t.pin_order = None;
                t.active_order = None;
            }
        }
        ThreadSnoozed { until } => {
            state
                .thread
                .as_mut()
                .ok_or(FoldError::Missing("thread"))?
                .snoozed_until = until.clone()
        }
        ThreadPinned { pinned, order } => {
            let t = state.thread.as_mut().ok_or(FoldError::Missing("thread"))?;
            t.pinned_at = pinned.then(|| at.clone());
            t.pin_order = order.clone();
            if *pinned {
                if t.settled == Some(true) {
                    t.settled = Some(false);
                    t.settled_at = None;
                }
                t.snoozed_until = None;
            }
        }
        ThreadActiveReordered { order } => {
            state
                .thread
                .as_mut()
                .ok_or(FoldError::Missing("thread"))?
                .active_order = Some(order.clone())
        }
        ThreadVisited { at } => {
            state
                .thread
                .as_mut()
                .ok_or(FoldError::Missing("thread"))?
                .last_visited_at = Some(at.clone())
        }
        AutoSettleChanged { enabled } => {
            state
                .thread
                .as_mut()
                .ok_or(FoldError::Missing("thread"))?
                .auto_settle = *enabled
        }
        ModelSelected { selection } => {
            state
                .thread
                .as_mut()
                .ok_or(FoldError::Missing("thread"))?
                .selection = selection.clone()
        }
        RuntimeModeChanged { mode } => {
            state
                .thread
                .as_mut()
                .ok_or(FoldError::Missing("thread"))?
                .runtime_mode = *mode
        }
        InteractionModeChanged { mode } => {
            state
                .thread
                .as_mut()
                .ok_or(FoldError::Missing("thread"))?
                .interaction_mode = *mode
        }
        MessageCreated {
            id,
            run,
            role,
            text,
            attachments,
            intent,
            created_by,
            creation_source,
        } => {
            if state.messages.iter().any(|m| &m.id == id) {
                return Err(FoldError::Conflict);
            }
            state.messages.push(Message {
                id: id.clone(),
                run: run.clone(),
                role: *role,
                text: text.clone(),
                attachments: attachments.clone(),
                intent: *intent,
                created_by: *created_by,
                creation_source: creation_source.clone(),
                streaming: *role == Role::Assistant,
                created_at: at.clone(),
                updated_at: at.clone(),
            });
        }
        MessageEdited {
            id,
            text,
            attachments,
        } => {
            let m = find_mut(&mut state.messages, "message", |m| &m.id == id)?;
            m.text = text.clone();
            if let Some(a) = attachments {
                m.attachments = a.clone();
            }
            m.updated_at = at.clone();
        }
        MessageAdopted { id, run, intent } => {
            let m = find_mut(&mut state.messages, "message", |m| &m.id == id)?;
            m.run = Some(run.clone());
            m.intent = *intent;
        }
        MessageFinished { id } => {
            let m = find_mut(&mut state.messages, "message", |m| &m.id == id)?;
            m.streaming = false;
            m.updated_at = at.clone();
        }
        RunRequested {
            id,
            message,
            ordinal,
            selection,
            status,
            queue_position,
            held,
            source_plan,
        } => {
            if state.runs.iter().any(|r| &r.id == id) {
                return Err(FoldError::Conflict);
            }
            state.runs.push(Run {
                id: id.clone(),
                message: message.clone(),
                ordinal: *ordinal,
                selection: selection.clone(),
                status: *status,
                attempt: None,
                queue_position: *queue_position,
                queue_held: *held,
                requested_at: at.clone(),
                started_at: None,
                completed_at: None,
                source_plan: source_plan.clone(),
                checkpoint: None,
                continuation: false,
            });
        }
        RunStarted { id } => {
            let r = find_mut(&mut state.runs, "run", |r| &r.id == id)?;
            r.status = RunStatus::Starting;
            r.queue_position = None;
            r.queue_held = false;
            r.started_at = Some(at.clone());
            r.completed_at = None;
        }
        RunPrepared { id } => {
            let r = find_mut(&mut state.runs, "run", |r| &r.id == id)?;
            r.status = RunStatus::Preparing;
            r.completed_at = None;
        }
        QueueHeld { id, held } => {
            find_mut(&mut state.runs, "run", |r| &r.id == id)?.queue_held = *held
        }
        QueueReordered { order } => {
            for (i, id) in order.iter().enumerate() {
                find_mut(&mut state.runs, "run", |r| &r.id == id)?.queue_position =
                    Some(i as u64 + 1);
            }
        }
        RunWaitingForCapture { id, terminal } => {
            let run = find_mut(&mut state.runs, "run", |r| &r.id == id)?;
            run.status = if matches!(terminal, RunStatus::Interrupted | RunStatus::Cancelled) {
                *terminal
            } else {
                RunStatus::Waiting
            };
            if run.status.terminal() {
                run.completed_at = Some(at.clone());
            }
            state.captures.insert(id.clone(), *terminal);
        }
        RunFinished { id, status } => {
            let r = find_mut(&mut state.runs, "run", |r| &r.id == id)?;
            r.status = *status;
            r.queue_position = None;
            r.completed_at = Some(at.clone());
            state.captures.remove(id);
            if state
                .pending_prompt
                .as_ref()
                .is_some_and(|p| r.attempt.as_ref() == Some(&p.attempt))
            {
                state.pending_prompt = None;
            }
        }
        AttemptStarted { id, run, ordinal } => {
            if state.attempts.iter().any(|a| &a.id == id) {
                return Err(FoldError::Conflict);
            }
            find_mut(&mut state.runs, "run", |r| &r.id == run)?.attempt = Some(id.clone());
            state.attempts.push(Attempt {
                id: id.clone(),
                run: run.clone(),
                ordinal: *ordinal,
                status: AttemptStatus::Pending,
                native_thread: None,
                native_turn: None,
                native_head: None,
                usage: None,
                started_at: at.clone(),
                completed_at: None,
            });
        }
        AttemptFinished { id, status } => {
            let a = find_mut(&mut state.attempts, "attempt", |a| &a.id == id)?;
            a.status = *status;
            a.completed_at = Some(at.clone());
        }
        SessionBound {
            attempt,
            native_thread,
        } => {
            find_mut(&mut state.attempts, "attempt", |a| &a.id == attempt)?.native_thread =
                Some(native_thread.clone())
        }
        TurnBound {
            attempt,
            native_turn,
        } => {
            let a = find_mut(&mut state.attempts, "attempt", |a| &a.id == attempt)?;
            a.native_turn = native_turn.clone();
            a.status = AttemptStatus::Running;
            let run = a.run.clone();
            find_mut(&mut state.runs, "run", |r| r.id == run)?.status = RunStatus::Running;
        }
        NativeHeadChanged { instance, head } => {
            state.native_heads.insert(instance.clone(), head.clone());
        }
        UsageRecorded { attempt, usage } => {
            find_mut(&mut state.attempts, "attempt", |a| &a.id == attempt)?.usage =
                Some(usage.clone())
        }
        ItemStarted {
            id,
            run,
            attempt,
            native_key,
            ordinal,
            kind,
        } => {
            if state.items.iter().any(|i| &i.id == id) {
                return Err(FoldError::Conflict);
            }
            state.items.push(Item {
                id: id.clone(),
                run: run.clone(),
                attempt: attempt.clone(),
                native_key: native_key.clone(),
                ordinal: *ordinal,
                kind: kind.clone(),
                status: ItemStatus::Running,
                text: String::new(),
                started_at: at.clone(),
                completed_at: None,
            });
        }
        ItemTextAppended { id, offset, text } => {
            let item = find_mut(&mut state.items, "item", |i| &i.id == id)?;
            if item.text.len() != *offset {
                return Err(FoldError::Offset);
            }
            item.text.push_str(text);
            if let Some(m) = message_for_item(state, id) {
                m.text.push_str(text);
                m.updated_at = at.clone();
            }
        }
        ItemTextReplaced { id, text } => {
            find_mut(&mut state.items, "item", |i| &i.id == id)?.text = text.clone();
            if let Some(m) = message_for_item(state, id) {
                m.text = text.clone();
                m.updated_at = at.clone();
            }
        }
        ItemDetailChanged { id, kind } => {
            find_mut(&mut state.items, "item", |i| &i.id == id)?.kind = kind.clone()
        }
        ItemCompleted { id, status } => {
            let item = find_mut(&mut state.items, "item", |i| &i.id == id)?;
            item.status = *status;
            item.completed_at = Some(at.clone());
            if let Some(m) = message_for_item(state, id) {
                m.streaming = false;
                m.updated_at = at.clone();
            }
        }
        RequestOpened {
            owner_path,
            id,
            attempt,
            native_key,
            body,
            capability,
        } => {
            if state.requests.iter().any(|r| &r.id == id) {
                return Err(FoldError::Conflict);
            }
            state.requests.push(Request {
                owner_path: owner_path.clone(),
                id: id.clone(),
                attempt: attempt.clone(),
                native_key: native_key.clone(),
                body: body.clone(),
                capability: *capability,
                status: RequestStatus::Pending,
                decision: None,
                answers: None,
                attachments: BTreeMap::new(),
                created_at: at.clone(),
                resolved_at: None,
            });
        }
        RequestResolved {
            id,
            status,
            decision,
            answers,
            attachments,
        } => {
            let r = find_mut(&mut state.requests, "request", |r| &r.id == id)?;
            r.status = *status;
            r.capability = if matches!(status, RequestStatus::Expired | RequestStatus::Cancelled) {
                ResponseCapability::NotResumable
            } else {
                r.capability
            };
            r.decision = *decision;
            r.answers = answers.clone();
            r.attachments = attachments.clone();
            r.resolved_at = Some(at.clone());
        }
        PlanStarted {
            id,
            run,
            native_key,
            kind,
        } => {
            if state.plans.iter().any(|p| &p.id == id) {
                return Err(FoldError::Conflict);
            }
            state.plans.push(Plan {
                id: id.clone(),
                kind: *kind,
                run: run.clone(),
                native_key: native_key.clone(),
                markdown: String::new(),
                steps: vec![],
                implemented_by: None,
            });
        }
        PlanMarkdownAppended { id, offset, text } => {
            let plan = find_mut(&mut state.plans, "plan", |p| &p.id == id)?;
            if plan.markdown.len() != *offset {
                return Err(FoldError::Offset);
            }
            plan.markdown.push_str(text);
        }
        PlanMarkdownReplaced { id, text } => {
            find_mut(&mut state.plans, "plan", |p| &p.id == id)?.markdown = text.clone();
        }
        PlanStepsReplaced { id, steps } => {
            find_mut(&mut state.plans, "plan", |p| &p.id == id)?.steps = steps.clone();
        }
        PlanImplemented { id, run } => {
            find_mut(&mut state.plans, "plan", |p| &p.id == id)?.implemented_by = Some(run.clone())
        }
        CheckpointCaptured {
            id,
            run,
            run_ordinal,
            native_heads,
            file_ref,
        } => {
            state.checkpoints.push(Checkpoint {
                id: id.clone(),
                run: run.clone(),
                run_ordinal: *run_ordinal,
                native_heads: native_heads.clone(),
                file_ref: file_ref.clone(),
            });
            if let Some(run) = run {
                find_mut(&mut state.runs, "run", |r| &r.id == run)?.checkpoint = Some(id.clone());
            }
        }
        RollbackRequested {
            command,
            checkpoint,
            restore_files,
        } => {
            state.rollback = Some(PendingRollback {
                command: command.clone(),
                checkpoint: checkpoint.clone(),
                restore_files: *restore_files,
            });
            state.rollback_failure = None;
        }
        RolledBack {
            command,
            checkpoint,
        } => {
            if state
                .rollback
                .as_ref()
                .is_none_or(|p| &p.command != command)
            {
                return Err(FoldError::Conflict);
            }
            let cp = state
                .checkpoints
                .iter()
                .find(|c| &c.id == checkpoint)
                .ok_or(FoldError::Missing("checkpoint"))?;
            for run in &mut state.runs {
                if run.ordinal > cp.run_ordinal {
                    run.status = RunStatus::RolledBack;
                    run.completed_at = Some(at.clone());
                }
            }
            state.native_heads = cp.native_heads.clone();
            state.rollback = None;
        }
        RollbackFailed { command, message } => {
            if state
                .rollback
                .as_ref()
                .is_none_or(|p| &p.command != command)
            {
                return Err(FoldError::Conflict);
            }
            state.rollback = None;
            state.rollback_failure = Some(message.clone());
        }
        ForkAccepted {
            parent,
            boundary,
            history,
        } => {
            let t = state.thread.as_mut().ok_or(FoldError::Missing("thread"))?;
            t.parent = Some(parent.clone());
            t.fork_boundary = Some(*boundary);
            state.inherited_items = history.clone();
        }
        TransferOpened {
            id,
            kind,
            source,
            target,
            boundary,
            text,
        } => {
            for transfer in &mut state.transfers {
                if transfer.source == *source
                    && transfer.target == *target
                    && transfer.kind == *kind
                    && transfer.consumed_by.is_none()
                {
                    transfer.superseded = true;
                }
            }
            state.transfers.push(Transfer {
                id: id.clone(),
                kind: *kind,
                source: source.clone(),
                target: target.clone(),
                boundary: *boundary,
                text: text.clone(),
                consumed_by: None,
                superseded: false,
            });
        }
        TransferConsumed { id, run } => {
            find_mut(&mut state.transfers, "transfer", |t| &t.id == id)?.consumed_by =
                Some(run.clone())
        }
        TaskStarted {
            id,
            native_key,
            run,
            attempt,
            child,
            parent,
            app_owned,
            prompt,
            model,
            wake,
        } => {
            if state.tasks.iter().any(|t| &t.id == id) {
                return Err(FoldError::Conflict);
            }
            state.tasks.push(Task {
                id: id.clone(),
                native_key: native_key.clone(),
                run: run.clone(),
                attempt: attempt.clone(),
                child_thread: child.clone(),
                parent_task: parent.clone(),
                app_owned: *app_owned,
                prompt: prompt.clone(),
                title: None,
                started_at: at.clone(),
                completed_at: None,
                model: model.clone(),
                wake: *wake,
                status: ItemStatus::Running,
                result: None,
                progress: None,
                delivery: DeliveryState::Pending,
            });
        }
        TaskProgressed {
            id,
            progress,
            model,
        } => {
            let task = find_mut(&mut state.tasks, "task", |t| &t.id == id)?;
            task.progress = Some(progress.clone());
            if model.is_some() {
                task.model = model.clone();
            }
        }
        TaskFinished { id, status, result } => {
            let task = find_mut(&mut state.tasks, "task", |t| &t.id == id)?;
            task.status = *status;
            task.result = Some(result.clone());
            task.completed_at = Some(at.clone());
        }
        TaskReopened { id } => {
            let t = find_mut(&mut state.tasks, "task", |t| &t.id == id)?;
            t.status = ItemStatus::Running;
            t.result = None;
            t.delivery = DeliveryState::Pending;
            t.completed_at = None;
        }
        TaskWakeChanged { id, wake } => {
            find_mut(&mut state.tasks, "task", |t| &t.id == id)?.wake = *wake
        }
        TaskDeliveryChanged {
            id,
            state: delivery,
        } => find_mut(&mut state.tasks, "task", |t| &t.id == id)?.delivery = *delivery,
    }
    if !matches!(fact.body, FactBody::ThreadVisited { .. })
        && let Some(thread) = &mut state.thread
    {
        thread.updated_at = at.clone();
    }
    Ok(())
}
pub fn fold(initial: &State, facts: &[Fact]) -> Result<State, FoldError> {
    let mut state = initial.clone();
    for fact in facts {
        apply(&mut state, fact)?;
    }
    Ok(state)
}
