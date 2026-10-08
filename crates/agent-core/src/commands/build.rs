//! Domain commands and launches built from what the user chose. The Host
//! resolves delivery against the live run, so sends carry the user's intent.
use super::workflows::interrupt_target;
use crate::js_text::utf16_len;
use crate::presentation::markdown::assistant_citations::assistant_citations_to_plain_text;
use agent_domain::{
    Answers, ApprovalDecision, Attachment, Checkpoint, CheckpointStatus, Command, CommandId,
    Continuation, DeliveryIntent, DispatchMode, InteractionMode, LimitRecoveryUpdate,
    MessageAuthor, MessageContext, MessageId, ModelSelection, PlanRef, RunId, RuntimeMode,
    RuntimeRequestId, SendMessage, SourcePoint, State, ThreadId, Timestamp,
};
use agent_protocol::conversation::{Dispatch, Launch, LaunchMessage, WorkspaceStrategy};
use std::collections::BTreeMap;

/// What a composer submission does while a turn is in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerDispatchMode {
    Auto,
    Queue,
    Steer,
    Restart,
}

/// The configured follow-up for a running turn; the alternate gesture swaps
/// queue and steer.
/// The setting defaults to queue; an unset behavior steers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum FollowUpBehavior {
    #[default]
    Queue,
    Steer,
    Restart,
}

pub fn resolve_composer_dispatch_mode(
    running: bool,
    alternate: bool,
    behavior: Option<FollowUpBehavior>,
) -> ComposerDispatchMode {
    if !running {
        return ComposerDispatchMode::Auto;
    }
    let behavior = behavior.unwrap_or(FollowUpBehavior::Steer);
    match (alternate, behavior) {
        (true, FollowUpBehavior::Queue) => ComposerDispatchMode::Steer,
        (true, _) => ComposerDispatchMode::Queue,
        (false, FollowUpBehavior::Queue) => ComposerDispatchMode::Queue,
        (false, FollowUpBehavior::Steer) => ComposerDispatchMode::Steer,
        (false, FollowUpBehavior::Restart) => ComposerDispatchMode::Restart,
    }
}

/// What the alternate gesture does, for labelling it.
pub fn alternate_follow_up(behavior: Option<FollowUpBehavior>) -> FollowUpBehavior {
    match resolve_composer_dispatch_mode(true, true, behavior) {
        ComposerDispatchMode::Steer => FollowUpBehavior::Steer,
        _ => FollowUpBehavior::Queue,
    }
}

/// How one message is delivered; `Start` begins a turn on an idle thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnDispatch {
    Auto,
    Queue,
    Steer,
    Restart,
    Start,
}
impl From<ComposerDispatchMode> for TurnDispatch {
    fn from(mode: ComposerDispatchMode) -> Self {
        match mode {
            ComposerDispatchMode::Auto => Self::Auto,
            ComposerDispatchMode::Queue => Self::Queue,
            ComposerDispatchMode::Steer => Self::Steer,
            ComposerDispatchMode::Restart => Self::Restart,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TurnMessage {
    pub id: MessageId,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub context: Option<MessageContext>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StartTurn {
    pub message: TurnMessage,
    pub selection: Option<ModelSelection>,
    pub title_seed: Option<String>,
    pub source_plan: Option<PlanRef>,
    pub dispatch: TurnDispatch,
    /// Continues this interrupted or usage-limited run; only for `Start`.
    pub continuation: Option<RunId>,
    pub creation_source: String,
}

pub fn send_command(input: StartTurn) -> Command {
    let (mode, intent) = match input.dispatch {
        TurnDispatch::Start => (DispatchMode::StartImmediately, None),
        TurnDispatch::Queue => (DispatchMode::QueueAfterActive, None),
        TurnDispatch::Auto => (DispatchMode::StartImmediately, Some(DeliveryIntent::Auto)),
        TurnDispatch::Steer => (DispatchMode::StartImmediately, Some(DeliveryIntent::Steer)),
        TurnDispatch::Restart => (
            DispatchMode::StartImmediately,
            Some(DeliveryIntent::Restart),
        ),
    };
    let continuation = input
        .continuation
        .filter(|_| input.dispatch == TurnDispatch::Start)
        .map(|run| Continuation::Manual { run });
    Command::Send(SendMessage {
        scheduled_task: None,
        created_by: MessageAuthor::User,
        creation_source: input.creation_source,
        id: input.message.id,
        text: input.message.text,
        attachments: input.message.attachments,
        selection: input.selection,
        mode,
        intent,
        source_plan: input.source_plan,
        resolved_plan: None,
        continuation,
        title_seed: input.title_seed,
        context: input.message.context,
    })
}

/// Where a new thread's first run works.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceChoice {
    Local {
        branch: Option<String>,
        worktree_path: Option<String>,
    },
    NewWorktree {
        base_branch: String,
        branch: Option<String>,
        start_from_origin: bool,
    },
}

pub fn workspace_strategy(choice: WorkspaceChoice) -> WorkspaceStrategy {
    match choice {
        WorkspaceChoice::NewWorktree {
            base_branch,
            branch,
            start_from_origin,
        } => WorkspaceStrategy::Worktree {
            base_ref: base_branch,
            branch,
            start_from_origin,
        },
        WorkspaceChoice::Local {
            branch,
            worktree_path: Some(worktree_path),
        } if !worktree_path.is_empty() => WorkspaceStrategy::ExistingWorktree {
            worktree_path,
            branch,
        },
        WorkspaceChoice::Local { branch, .. } => WorkspaceStrategy::Root { branch },
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LaunchThread {
    pub command_id: CommandId,
    pub thread: Option<ThreadId>,
    pub project: String,
    pub title: String,
    pub title_seed: Option<String>,
    pub selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub workspace: WorkspaceChoice,
    pub message: Option<TurnMessage>,
    pub creation_source: String,
}

/// A new thread with its first message; the seed is its title until one is
/// generated.
pub fn launch(input: LaunchThread) -> Launch {
    Launch {
        command_id: input.command_id,
        thread_id: input.thread,
        project_id: input.project,
        title: input.title_seed.clone().unwrap_or(input.title),
        selection: input.selection,
        runtime_mode: input.runtime_mode,
        interaction_mode: input.interaction_mode,
        workspace: workspace_strategy(input.workspace),
        message: input.message.map(|message| LaunchMessage {
            id: Some(message.id),
            text: message.text,
            attachments: message.attachments,
            creation_source: input.creation_source,
            title_seed: input.title_seed,
            context: message.context,
        }),
    }
}

pub fn dispatch(thread: ThreadId, command_id: CommandId, command: Command) -> Dispatch {
    Dispatch {
        thread_id: thread,
        command_id,
        command,
    }
}

/// Stop interrupts the given run, or the run `interrupt_target` names, and
/// holds the queue.
pub fn interrupt_command(state: &State, run: Option<RunId>) -> Option<Command> {
    Some(Command::Interrupt {
        run: run.or_else(|| interrupt_target(state))?,
        hold_queue: true,
        reason: None,
    })
}

pub fn rollback_command(checkpoint: &Checkpoint, restore_files: bool) -> Command {
    Command::Rollback {
        checkpoint: checkpoint.id.clone(),
        restore_files,
        restore_refusal: None,
    }
}

/// The checkpoint after run `ordinal` (0 is the thread start), which must be
/// ready to roll back to.
pub fn checkpoint_after_run(state: &State, ordinal: u64) -> Result<&Checkpoint, String> {
    state
        .checkpoints
        .iter()
        .rev()
        .find(|checkpoint| checkpoint.run_ordinal == ordinal)
        .filter(|checkpoint| checkpoint.status == CheckpointStatus::Ready)
        .ok_or_else(|| format!("run ordinal {ordinal} has no ready checkpoint"))
}

/// One detach per provider instance the thread holds a session for.
pub fn detach_commands(state: &State, base: &CommandId) -> Vec<(CommandId, Command)> {
    state
        .native_sessions
        .keys()
        .filter_map(|instance| {
            Some((
                CommandId::new(format!("{base}:detach:{instance}")).ok()?,
                Command::DetachProviderSession {
                    instance: instance.clone(),
                    reason: Some("client-requested".into()),
                },
            ))
        })
        .collect()
}

pub fn fork_command(
    target: ThreadId,
    run: RunId,
    title: Option<String>,
    creation_source: &str,
) -> Command {
    Command::Fork {
        target,
        source: SourcePoint::Run(run),
        title,
        created_by: MessageAuthor::User,
        creation_source: creation_source.into(),
    }
}

pub fn merge_back_command(target: ThreadId, run: RunId) -> Command {
    Command::MergeBack {
        target,
        source: SourcePoint::Run(run),
    }
}

/// A metadata change; omitted fields keep their value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MetadataChange {
    pub title: Option<String>,
    pub regenerate_title: Option<bool>,
    pub limit_recovery: Option<Option<LimitRecoveryUpdate>>,
}

pub fn metadata_command(change: MetadataChange) -> Command {
    Command::UpdateMetadata {
        title: change.title,
        regenerate_title: change.regenerate_title,
        branch: None,
        worktree_path: None,
        expected_worktree_path: None,
        expected_empty: false,
        limit_recovery: change.limit_recovery,
        linked_pull_request: None,
        project_root: None,
    }
}

pub fn select_model_command(selection: ModelSelection) -> Command {
    Command::SelectModel { selection }
}

/// Thread list and lifecycle actions.
#[derive(Debug, Clone, PartialEq)]
pub enum LifecycleAction {
    Pin { order: Option<String> },
    Unpin,
    ReorderPinned { order: String },
    ReorderActive { order: String },
    Settle,
    Unsettle,
    Snooze { until: Timestamp },
    Unsnooze,
    AutoSettle { enabled: bool },
    Archive,
    Unarchive,
    Delete,
    Rename { title: String },
    RegenerateTitle,
    MarkUnread,
    Visit { at: Timestamp },
}

pub fn lifecycle_command(action: LifecycleAction) -> Command {
    match action {
        LifecycleAction::Pin { order } => Command::Pin {
            pinned: true,
            order,
        },
        LifecycleAction::Unpin => Command::Pin {
            pinned: false,
            order: None,
        },
        LifecycleAction::ReorderPinned { order } => Command::ReorderPinned { order },
        LifecycleAction::ReorderActive { order } => Command::ReorderActive { order },
        LifecycleAction::Settle => Command::Settle {
            settled: true,
            at: None,
        },
        LifecycleAction::Unsettle => Command::Settle {
            settled: false,
            at: None,
        },
        LifecycleAction::Snooze { until } => Command::Snooze { until: Some(until) },
        LifecycleAction::Unsnooze => Command::Snooze { until: None },
        LifecycleAction::AutoSettle { enabled } => Command::AutoSettle { enabled },
        LifecycleAction::Archive => Command::Archive { archived: true },
        LifecycleAction::Unarchive => Command::Archive { archived: false },
        LifecycleAction::Delete => Command::Delete,
        LifecycleAction::Rename { title } => metadata_command(MetadataChange {
            title: Some(title),
            ..MetadataChange::default()
        }),
        LifecycleAction::RegenerateTitle => metadata_command(MetadataChange {
            regenerate_title: Some(true),
            ..MetadataChange::default()
        }),
        LifecycleAction::MarkUnread => Command::MarkUnread,
        LifecycleAction::Visit { at } => Command::Visit { at },
    }
}

pub fn approval_command(request: RuntimeRequestId, decision: ApprovalDecision) -> Command {
    Command::Respond {
        request,
        decision: Some(decision),
        answers: None,
        attachments: BTreeMap::new(),
    }
}

pub fn answers_command(
    request: RuntimeRequestId,
    answers: Answers,
    attachments: BTreeMap<String, Vec<Attachment>>,
) -> Command {
    Command::Respond {
        request,
        decision: None,
        answers: Some(answers),
        attachments,
    }
}

const TITLE_MAX: usize = 50;

fn truncate_title(text: &str) -> String {
    let text = text.trim();
    if utf16_len(text) <= TITLE_MAX {
        return text.into();
    }
    let mut units = 0;
    let head: String = text
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= TITLE_MAX
        })
        .collect();
    format!("{head}...")
}

/// Quotes read as their text and comment; whitespace runs collapse to one space.
fn normalize_title_seed(text: &str) -> String {
    assistant_citations_to_plain_text(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The title shown while the first message's title is generated.
pub fn thread_title_seed(
    text: &str,
    attachment_names: &[&str],
    fallbacks: &[Option<&str>],
) -> String {
    let text = normalize_title_seed(text);
    if !text.is_empty() {
        return truncate_title(&text);
    }
    let attachment = normalize_title_seed(attachment_names.first().copied().unwrap_or(""));
    if !attachment.is_empty() {
        return truncate_title(&format!("Image: {attachment}"));
    }
    fallbacks
        .iter()
        .map(|label| normalize_title_seed(label.unwrap_or("")))
        .find(|label| !label.is_empty())
        .map_or_else(|| "New thread".into(), |label| truncate_title(&label))
}

pub const PLAN_IMPLEMENTATION_PROMPT_PREFIX: &str = "PLEASE IMPLEMENT THIS PLAN:\n";

/// The heading text of an ATX heading line (`#` to `######` after up to three spaces).
pub(crate) fn atx_heading(line: &str) -> Option<&str> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let rest = &line[indent..];
    let hashes = rest.len() - rest.trim_start_matches('#').len();
    let after = &rest[hashes..];
    (indent <= 3 && (1..=6).contains(&hashes) && after.starts_with(char::is_whitespace))
        .then(|| after.trim())
}

pub fn proposed_plan_title(markdown: &str) -> Option<String> {
    markdown.lines().find_map(|line| {
        atx_heading(line)
            .filter(|title| !title.is_empty())
            .map(str::to_owned)
    })
}

pub fn plan_implementation_prompt(markdown: &str) -> String {
    format!("{PLAN_IMPLEMENTATION_PROMPT_PREFIX}{}", markdown.trim())
}

/// The draft refines the plan; an empty draft implements it.
pub fn plan_follow_up(draft_text: &str, markdown: &str) -> (String, InteractionMode) {
    let draft = draft_text.trim();
    if draft.is_empty() {
        (
            plan_implementation_prompt(markdown),
            InteractionMode::Default,
        )
    } else {
        (draft.into(), InteractionMode::Plan)
    }
}

pub fn plan_implementation_thread_title(markdown: &str) -> String {
    proposed_plan_title(markdown)
        .map_or_else(|| "Implement plan".into(), |t| format!("Implement {t}"))
}

#[cfg(test)]
#[path = "build_tests.rs"]
mod tests;
