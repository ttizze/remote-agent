use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fact {
    pub at: Timestamp,
    pub body: FactBody,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FactBody {
    RestartContinuationLinked {
        run: RunId,
        source: RunId,
    },
    RunBackgroundWorkCancelled {
        run: RunId,
        work: Vec<CancelledBackgroundWork>,
    },
    NativeChildClosed,
    CheckpointScopeBound {
        run: Option<RunId>,
        scope: Option<CheckpointScope>,
    },
    NativeContextUsageRecorded {
        usage: ContextUsage,
    },
    NativeTurnUsageRecorded {
        usage: TurnTokenUsage,
    },
    NativeUsageAdded {
        counters: UsageCounters,
    },
    UsageBaselineChanged {
        native_thread: String,
        counters: UsageCounters,
    },
    UsageAdded {
        attempt: RunAttemptId,
        counters: UsageCounters,
    },
    TurnUsageRecorded {
        attempt: RunAttemptId,
        usage: TurnTokenUsage,
    },
    HandoffPolicyChanged {
        instance: String,
        model_window: Option<u64>,
        token_cap: u64,
    },
    NativeSessionCleared {
        instance: String,
    },
    ContextUsageRecorded {
        attempt: RunAttemptId,
        usage: ContextUsage,
    },
    TaskNamed {
        id: NodeId,
        title: String,
    },
    ForkSessionReserved {
        attempt: RunAttemptId,
        native_thread: String,
    },
    NativeSessionBound {
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
    },
    BackgroundWorkStopped,
    NativeWorkReported {
        key: String,
        report: WorkReport,
        text: String,
    },
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
        generation: u64,
    },
    NativeChildTurnBound {
        native_turn: Option<String>,
    },
    ThreadRenamed {
        title: String,
    },
    ThreadArranged(ThreadArrangement),
    LimitRecoveryChanged {
        recovery: Option<LimitRecovery>,
    },
    PullRequestLinked {
        pull_request: Option<LinkedPullRequest>,
    },
    RateLimitRejected {
        attempt: RunAttemptId,
        limit: String,
        resets_at: Option<i64>,
    },
    RateLimitsReported {
        instance: String,
        resets_at: Option<i64>,
    },
    ThreadUnsettled,
    ThreadImported,
    WorkspaceBound {
        workspace: Option<Workspace>,
    },
    TitleRequested {
        request: CommandId,
    },
    TitleRequestCleared,
    RunRestarting {
        id: RunId,
        selection: ModelSelection,
    },
    ItemMoved {
        id: TurnItemId,
        run: RunId,
        ordinal: Option<u64>,
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
    ThreadPinReordered {
        order: String,
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
    MessageNotificationAssigned {
        id: MessageId,
        notification: Notification,
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
        source_plan: Option<PlanRef>,
    },
    RunStarted {
        checkpoint_scope: Option<CheckpointScope>,
        native_baseline_heads: BTreeMap<String, Option<String>>,
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
    ItemReopened {
        id: TurnItemId,
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
        status: CheckpointStatus,
        scope: Option<CheckpointScope>,
        id: CheckpointId,
        run: Option<RunId>,
        run_ordinal: u64,
        native_heads: BTreeMap<String, Option<String>>,
        file_ref: String,
        files: Vec<CheckpointFile>,
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
    RollbackRewindStarted {
        command: CommandId,
        instances: Vec<String>,
    },
    ForkAccepted {
        parent: ThreadId,
        boundary: u64,
        history: Vec<Item>,
        messages: Vec<Message>,
    },
    TransferOpened {
        native_source: Option<NativeBinding>,
        id: ContextTransferId,
        kind: TransferKind,
        source: ThreadId,
        target: ThreadId,
        instance: Option<String>,
        target_run: Option<RunId>,
        boundary: u64,
        history: HistoricalContext,
    },
    TransferDeliveryChanged {
        id: ContextTransferId,
        delivery: ContextDelivery,
    },
    TaskNativeBound {
        id: NodeId,
        native_task: String,
    },
    DelegationAccepted {
        origin: Delegation,
    },
    TaskStarted {
        original_message: Option<MessageId>,
        background: bool,
        id: NodeId,
        native_key: String,
        run: Option<RunId>,
        attempt: RunAttemptId,
        child: ThreadId,
        parent: Option<NodeId>,
        prompt: String,
        model: Option<String>,
        wake: CompletionWake,
    },
    TaskProgressed {
        id: NodeId,
        progress: Option<String>,
        model: Option<String>,
    },
    TaskFinished {
        id: NodeId,
        status: ItemStatus,
        result: String,
    },
    TaskReopened {
        id: NodeId,
        run: Option<RunId>,
        attempt: RunAttemptId,
        prompt: String,
    },
    TaskWakeChanged {
        id: NodeId,
        wake: CompletionWake,
    },
    TaskDeliveryChanged {
        id: NodeId,
        state: DeliveryState,
    },
    /// Delivery only, never stored: a tool item as clients receive it, standing in
    /// for the fact that started or changed it (T3 WireProjection `turn-item.updated`).
    ItemProjected {
        item: Item,
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
        CheckpointScopeBound { run, scope } => {
            if let Some(run) = run {
                find_mut(&mut state.runs, "run", |candidate| &candidate.id == run)?
                    .checkpoint_scope = scope.clone();
            }
            if run.as_ref().is_none_or(|id| {
                state.active_run().is_some_and(|active| &active.id == id)
                    || state.active_run().is_none()
                        && state.runs.last().is_some_and(|latest| &latest.id == id)
            }) {
                state.checkpoint_scope = scope.clone();
            }
        }

        NativeContextUsageRecorded { usage } => state.native_context_usage = Some(usage.clone()),
        NativeTurnUsageRecorded { usage } => state.native_turn_usage = Some(usage.clone()),
        NativeUsageAdded { counters } => {
            state.native_usage_observed |= counters.observed();
            state.native_usage_accumulator =
                Some(add_usage(state.native_usage_accumulator.as_ref(), counters));
        }
        UsageBaselineChanged {
            native_thread,
            counters,
        } => {
            state
                .usage_baselines
                .insert(native_thread.clone(), counters.clone());
        }
        UsageAdded { attempt, counters } => {
            let attempt = find_mut(&mut state.attempts, "attempt", |a| &a.id == attempt)?;
            attempt.usage_observed |= counters.observed();
            attempt.usage_accumulator =
                Some(add_usage(attempt.usage_accumulator.as_ref(), counters));
        }
        TurnUsageRecorded { attempt, usage } => {
            find_mut(&mut state.attempts, "attempt", |a| &a.id == attempt)?.turn_usage =
                Some(usage.clone());
        }

        RestartContinuationLinked { run, source } => {
            find_mut(&mut state.runs, "run", |r| &r.id == run)?.restart_of = Some(source.clone());
        }
        RunBackgroundWorkCancelled { run, work } => {
            let target = find_mut(&mut state.runs, "run", |r| &r.id == run)?;
            for entry in work {
                if !target
                    .restart_cancelled_work
                    .iter()
                    .any(|existing| existing.id == entry.id)
                {
                    target.restart_cancelled_work.push(entry.clone());
                }
            }
        }
        NativeChildClosed => {
            state.native_owner = None;
            state.native_child_turn = None;
        }
        HandoffPolicyChanged {
            instance,
            model_window,
            token_cap,
        } => {
            state.handoff_token_cap = Some((*token_cap).clamp(1024, 64_000));
            if let Some(window) = model_window {
                state.context_windows.insert(instance.clone(), *window);
            } else {
                state.context_windows.remove(instance);
            }
        }
        NativeSessionCleared { instance } => {
            if let Some(native_thread) = state.native_sessions.remove(instance) {
                state.usage_baselines.remove(&native_thread);
            }
            state.native_heads.remove(instance);
        }
        ContextUsageRecorded { attempt, usage } => {
            find_mut(&mut state.attempts, "attempt", |a| &a.id == attempt)?.context_usage =
                Some(usage.clone());
        }

        TaskNamed { id, title } => {
            find_mut(&mut state.tasks, "task", |t| &t.id == id)?.title = Some(title.clone())
        }
        ForkSessionReserved { .. } => {}
        NativeSessionBound {
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
        BackgroundTaskFinished { key } => {
            state.background_work.remove(key);
        }
        BackgroundWorkStopped => {
            state.background_work.clear();
            state.wake_reports.clear();
        }
        NativeWorkReported { key, report, text } => {
            let recorded = WakeReport {
                key: key.clone(),
                report: report.clone(),
                text: text.clone(),
                prompt_ordinal: state.prompt_ordinal,
            };
            if let Some(existing) = state.wake_reports.iter_mut().find(|r| &r.key == key) {
                *existing = recorded;
            } else {
                state.wake_reports.push(recorded);
            }
        }
        WakeReportsConsumed => state.wake_reports.clear(),
        PromptOffered { attempt, key } => {
            state.prompt_ordinal += 1;
            state
                .wake_reports
                .retain(|report| report.prompt_ordinal >= state.prompt_ordinal - 1);
            state.pending_prompt = Some(PendingPrompt {
                attempt: attempt.clone(),
                key: key.clone(),
                confirmed: false,
                frames_before_echo: 0,
                held: vec![],
            });
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
                workspace: None,
                title_request: None,
                imported: false,
                snoozed_at: None,
                limit_recovery: None,
                linked_pull_request: None,
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
            generation,
        } => {
            state.native_generation = *generation;
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
            state.native_usage_accumulator = None;
            state.native_usage_observed = false;
            state.native_turn_usage = None;
        }
        ThreadRenamed { title } => {
            let thread = state.thread.as_mut().ok_or(FoldError::Missing("thread"))?;
            thread.title = title.clone();
            thread.title_request = None;
        }
        LimitRecoveryChanged { recovery } => {
            state
                .thread
                .as_mut()
                .ok_or(FoldError::Missing("thread"))?
                .limit_recovery = recovery.clone()
        }
        PullRequestLinked { pull_request } => {
            state
                .thread
                .as_mut()
                .ok_or(FoldError::Missing("thread"))?
                .linked_pull_request = pull_request.clone()
        }
        RateLimitRejected {
            attempt,
            limit,
            resets_at,
        } => {
            find_mut(&mut state.attempts, "attempt", |a| &a.id == attempt)?
                .rejected_limits
                .insert(limit.clone(), *resets_at);
        }
        RateLimitsReported {
            instance,
            resets_at,
        } => {
            state.rate_limit_resets.insert(instance.clone(), *resets_at);
        }
        ThreadArranged(arrangement) => {
            let t = state.thread.as_mut().ok_or(FoldError::Missing("thread"))?;
            t.pinned_at = arrangement.pinned_at.clone();
            t.pin_order = arrangement.pin_order.clone();
            t.active_order = arrangement.active_order.clone();
            t.auto_settle = arrangement.auto_settle;
            t.title_request = arrangement.title_request.clone();
        }
        ThreadImported => {
            state
                .thread
                .as_mut()
                .ok_or(FoldError::Missing("thread"))?
                .imported = true
        }
        WorkspaceBound { workspace } => {
            state
                .thread
                .as_mut()
                .ok_or(FoldError::Missing("thread"))?
                .workspace = workspace.clone()
        }
        TitleRequested { request } => {
            state
                .thread
                .as_mut()
                .ok_or(FoldError::Missing("thread"))?
                .title_request = Some(request.clone())
        }
        TitleRequestCleared => {
            state
                .thread
                .as_mut()
                .ok_or(FoldError::Missing("thread"))?
                .title_request = None
        }
        ThreadUnsettled => {
            let t = state.thread.as_mut().ok_or(FoldError::Missing("thread"))?;
            t.settled = None;
            t.settled_at = None;
        }
        RunRestarting { id, selection } => {
            let run = find_mut(&mut state.runs, "run", |r| &r.id == id)?;
            run.status = RunStatus::Starting;
            run.selection = selection.clone();
        }
        ItemMoved { id, run, ordinal } => {
            let item = find_mut(&mut state.items, "item", |i| &i.id == id)?;
            item.run = Some(run.clone());
            if let Some(ordinal) = ordinal {
                item.ordinal = *ordinal;
            }
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
            let t = state.thread.as_mut().ok_or(FoldError::Missing("thread"))?;
            t.snoozed_until = until.clone();
            t.snoozed_at = until.as_ref().map(|_| at.clone());
        }
        ThreadPinned { pinned, order } => {
            let t = state.thread.as_mut().ok_or(FoldError::Missing("thread"))?;
            if !pinned {
                t.pinned_at = None;
                t.pin_order = None;
            } else if t.pinned_at.is_none() {
                t.pinned_at = Some(at.clone());
                if order.is_some() {
                    t.pin_order = order.clone();
                }
            }
            if *pinned {
                if t.settled == Some(true) {
                    t.settled = Some(false);
                    t.settled_at = None;
                }
                t.snoozed_until = None;
                t.snoozed_at = None;
            }
        }
        ThreadPinReordered { order } => {
            state
                .thread
                .as_mut()
                .ok_or(FoldError::Missing("thread"))?
                .pin_order = Some(order.clone())
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
                notification: None,
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
        MessageNotificationAssigned { id, notification } => {
            find_mut(&mut state.messages, "message", |m| &m.id == id)?.notification =
                Some(notification.clone());
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
                restart_of: None,
                restart_cancelled_work: vec![],
                checkpoint_scope: None,
                native_baseline_heads: BTreeMap::new(),
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
        RunStarted {
            id,
            native_baseline_heads,
            checkpoint_scope,
        } => {
            let r = find_mut(&mut state.runs, "run", |r| &r.id == id)?;
            r.checkpoint_scope = checkpoint_scope.clone();
            r.native_baseline_heads = native_baseline_heads.clone();
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
                accepted: false,
                usage: None,
                context_usage: None,
                turn_usage: None,
                usage_accumulator: None,
                usage_observed: false,
                rejected_limits: BTreeMap::new(),
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
            let a = find_mut(&mut state.attempts, "attempt", |a| &a.id == attempt)?;
            a.native_thread = Some(native_thread.clone());
            let run = a.run.clone();
            let instance = find_mut(&mut state.runs, "run", |r| r.id == run)?
                .selection
                .instance
                .clone();
            state
                .native_sessions
                .insert(instance, native_thread.clone());
            for transfer in &mut state.transfers {
                if let Some(delivery) = &mut transfer.delivery
                    && &delivery.attempt == attempt
                    && delivery.status == ContextDeliveryStatus::Pending
                {
                    delivery.native_thread = Some(native_thread.clone());
                }
            }
        }
        TurnBound {
            attempt,
            native_turn,
        } => {
            let a = find_mut(&mut state.attempts, "attempt", |a| &a.id == attempt)?;
            a.native_turn = native_turn.clone();
            a.status = AttemptStatus::Running;
            a.accepted = true;
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
                output_omitted: false,
                output_indicates_failure: false,
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
        ItemReopened { id } => {
            let item = find_mut(&mut state.items, "item", |i| &i.id == id)?;
            item.status = ItemStatus::Running;
            item.completed_at = None;
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
            status,
            scope,
            id,
            run,
            run_ordinal,
            native_heads,
            file_ref,
            files,
        } => {
            let checkpoint = Checkpoint {
                status: *status,
                scope: scope.clone(),
                id: id.clone(),
                run: run.clone(),
                run_ordinal: *run_ordinal,
                native_heads: native_heads.clone(),
                file_ref: file_ref.clone(),
                files: files.clone(),
            };
            if let Some(existing) = state.checkpoints.iter_mut().find(|c| &c.id == id) {
                *existing = checkpoint;
            } else {
                state.checkpoints.push(checkpoint);
            }
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
                rewinding: BTreeSet::new(),
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
            let (target, scope) = (cp.run_ordinal, cp.scope.clone());
            state.native_heads = cp.native_heads.clone();
            for run in &mut state.runs {
                if run.ordinal > target
                    && run.status.terminal()
                    && run.status != RunStatus::RolledBack
                {
                    run.status = RunStatus::RolledBack;
                    run.completed_at = Some(at.clone());
                    state.captures.remove(&run.id);
                }
            }
            for checkpoint in &mut state.checkpoints {
                if checkpoint.scope == scope
                    && checkpoint.run_ordinal > target
                    && checkpoint.status == CheckpointStatus::Ready
                {
                    checkpoint.status = CheckpointStatus::Stale;
                }
            }
            state.rollback = None;
        }
        RollbackRewindStarted { command, instances } => {
            let pending = state
                .rollback
                .as_mut()
                .filter(|p| &p.command == command)
                .ok_or(FoldError::Conflict)?;
            pending.rewinding.extend(instances.iter().cloned());
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
            messages,
        } => {
            let t = state.thread.as_mut().ok_or(FoldError::Missing("thread"))?;
            t.parent = Some(parent.clone());
            t.fork_boundary = Some(*boundary);
            state.inherited_items = history.clone();
            state.inherited_messages = messages.clone();
        }
        TransferOpened {
            native_source,
            id,
            kind,
            source,
            target,
            instance,
            target_run,
            boundary,
            history,
        } => {
            for transfer in &mut state.transfers {
                if transfer.source == *source
                    && transfer.target == *target
                    && (transfer.kind == *kind
                        || matches!(
                            (transfer.kind, *kind),
                            (
                                TransferKind::ProviderHandoff,
                                TransferKind::ProviderHandoffDelta
                            ) | (
                                TransferKind::ProviderHandoffDelta,
                                TransferKind::ProviderHandoff
                            )
                        ))
                    && transfer.instance == *instance
                    && transfer.delivery.is_none()
                {
                    transfer.superseded = true;
                }
            }
            state.transfers.push(Transfer {
                native_source: native_source.clone(),
                id: id.clone(),
                kind: *kind,
                source: source.clone(),
                target: target.clone(),
                instance: instance.clone(),
                target_run: target_run.clone(),
                boundary: *boundary,
                history: history.clone(),
                delivery: None,
                superseded: false,
            });
        }
        TransferDeliveryChanged { id, delivery } => {
            let instance = state
                .runs
                .iter()
                .find(|run| run.id == delivery.run)
                .map(|run| run.selection.instance.clone());
            let transfer = find_mut(&mut state.transfers, "transfer", |transfer| {
                &transfer.id == id
            })?;
            transfer.delivery = Some(delivery.clone());
            if transfer.instance.is_none() {
                transfer.instance = instance;
            }
        }
        TaskNativeBound { id, native_task } => {
            find_mut(&mut state.tasks, "task", |t| &t.id == id)?.native_task =
                Some(native_task.clone());
        }
        DelegationAccepted { origin } => {
            state
                .thread
                .as_mut()
                .ok_or(FoldError::Missing("thread"))?
                .parent = Some(origin.parent.clone());
            state.delegation = Some(origin.clone());
        }
        TaskStarted {
            original_message,
            background,
            id,
            native_key,
            run,
            attempt,
            child,
            parent,
            prompt,
            model,
            wake,
        } => {
            if state.tasks.iter().any(|t| &t.id == id) {
                return Err(FoldError::Conflict);
            }
            state.tasks.push(Task {
                original_message: original_message.clone(),
                native_task: None,
                background: *background,
                id: id.clone(),
                native_key: native_key.clone(),
                run: run.clone(),
                attempt: attempt.clone(),
                child_thread: child.clone(),
                parent_task: parent.clone(),
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
                generation: 0,
            });
        }
        TaskProgressed {
            id,
            progress,
            model,
        } => {
            let task = find_mut(&mut state.tasks, "task", |t| &t.id == id)?;
            if progress.is_some() {
                task.progress = progress.clone();
            }
            if model.is_some() {
                task.model = model.clone();
            }
        }
        TaskFinished { id, status, result } => {
            let task = find_mut(&mut state.tasks, "task", |t| &t.id == id)?;
            task.status = *status;
            if !result.is_empty() {
                task.result = Some(result.clone());
            }
            task.completed_at = Some(at.clone());
        }
        TaskReopened {
            id,
            run,
            attempt,
            prompt,
        } => {
            let t = find_mut(&mut state.tasks, "task", |t| &t.id == id)?;
            t.status = ItemStatus::Running;
            t.run = run.clone();
            t.attempt = attempt.clone();
            t.prompt = prompt.clone();
            t.result = None;
            t.progress = None;
            t.started_at = at.clone();
            t.delivery = DeliveryState::Pending;
            t.completed_at = None;
            t.generation += 1;
            for item in &mut state.items {
                if matches!(&item.kind, ItemKind::Subagent { task } if task == id) {
                    item.status = ItemStatus::Running;
                    item.run = run.clone();
                    item.attempt = Some(attempt.clone());
                    item.started_at = at.clone();
                    item.completed_at = None;
                }
            }
        }
        TaskWakeChanged { id, wake } => {
            find_mut(&mut state.tasks, "task", |t| &t.id == id)?.wake = *wake
        }
        TaskDeliveryChanged {
            id,
            state: delivery,
        } => find_mut(&mut state.tasks, "task", |t| &t.id == id)?.delivery = *delivery,
        ItemProjected { item } => match state.items.iter_mut().find(|i| i.id == item.id) {
            Some(existing) => *existing = item.clone(),
            None => state.items.push(item.clone()),
        },
    }
    // Visits and arranging the active list are not thread activity (T3).
    if !matches!(
        fact.body,
        FactBody::ThreadVisited { .. } | FactBody::ThreadActiveReordered { .. }
    ) && let Some(thread) = &mut state.thread
    {
        thread.updated_at = at.clone();
    }
    Ok(())
}
/// Largest text carried by one fact. Longer text is split across appends so
/// each fact fits a transport frame; nothing is truncated.
pub const MAX_FACT_TEXT: usize = 1 << 20;
pub fn text_chunks(text: &str) -> Vec<&str> {
    let mut chunks = vec![];
    let mut rest = text;
    while !rest.is_empty() {
        let mut end = rest.len().min(MAX_FACT_TEXT);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push(&rest[..end]);
        rest = &rest[end..];
    }
    chunks
}
/// Version of the folded `State` and `Fact` encodings. Stored snapshots with
/// another value are rebuilt from facts.
pub const STATE_FORMAT: u32 = 2;
pub fn fold(initial: &State, facts: &[Fact]) -> Result<State, FoldError> {
    let mut state = initial.clone();
    for fact in facts {
        apply(&mut state, fact)?;
    }
    Ok(state)
}
