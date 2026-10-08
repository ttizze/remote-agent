//! The rows a client draws for one thread. The layout picks the desktop or
//! the mobile derivation; both produce the same row type.
use super::changed_files::{ChangedFilesCard, assistant_turn_diffs, changed_files_card};
use super::desktop::{DesktopRow, DesktopTimelineInput, TimelineLatestRun, derive_desktop_rows};
use super::desktop_labels::{live_work_entry_label, should_preserve_assistant_line_breaks};
use super::desktop_work_row::{WorkRowContext, desktop_work_log_row, tool_group_action_icon};
use super::entries::{ChatMessage, EntriesInput, derive_timeline_entries};
use super::lifecycle::{HandoffDivider, LifecycleRow, lifecycle_row};
use super::message::{
    AssistantMeta, IntentBadge, UserMessageDecorations, assistant_display_text, assistant_meta,
    user_message_decorations, user_message_intent_badge,
};
use super::mobile::{
    FeedInput, FeedLatestRun, FeedRow, FeedVisibility, build_thread_feed, feed_activity,
};
use super::mobile_presentation::derive_thread_feed_presentation;
use super::mobile_work_log::{
    SubagentGroupCard, WorkLogLayout, WorkToggleIcon, subagent_group_card, work_log_layout,
    work_log_row, work_toggle_presentation,
};
use super::pending::append_pending_messages;
use super::plan_card::{PlanCard, plan_card};
use super::splice::{Splice, changed, splice};
use super::timing::working_started_at;
use super::work_row::{WorkIcon, WorkLogRow, WorkRowIcon};
use crate::commands::outbox::{PendingMessage, Phase};
use crate::sync::{Detail, ThreadSync};
use crate::view::work_log::ToolIcon;
use crate::view::work_log::presentation::ToolGroupSummaryKind;
use agent_domain::{
    Attachment, CommandId, InputIntent, Item, ItemKind, Message, MessageAuthor, MessageContext,
    MessageId, RunAttemptId, RunId, State, ThreadId, ThreadShell, Timestamp, WorktreeSetupSnapshot,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum TimelineLayout {
    #[default]
    Desktop,
    Mobile,
}

/// Folder disclosure of one changed-files card.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangedFilesExpansion {
    pub all_expanded: bool,
    pub overrides: BTreeMap<String, bool>,
}

/// The client's disclosure state and preferences the rows depend on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimelineOptions {
    pub layout: TimelineLayout,
    pub expanded_runs: BTreeSet<RunId>,
    pub expanded_attempts: BTreeSet<RunAttemptId>,
    pub expanded_work_groups: BTreeSet<String>,
    /// Work-log entries whose detail is open, by entry id.
    pub expanded_entries: BTreeSet<String>,
    pub changed_files: BTreeMap<RunId, ChangedFilesExpansion>,
    pub workspace_root: Option<String>,
    /// A rollback is in progress; "Edit from here" waits for it.
    pub reverting: bool,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct UserMessageRow {
    pub message: MessageId,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub context: Option<MessageContext>,
    pub decorations: UserMessageDecorations,
    /// The mobile badge of a message that did not start a turn.
    pub badge: Option<IntentBadge>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AssistantMessageRow {
    pub message: MessageId,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub streaming: bool,
    pub preserve_line_breaks: bool,
    /// When the response's elapsed time counts from.
    pub duration_start: Option<Timestamp>,
    pub meta: Option<AssistantMeta>,
    pub changed_files: Option<ChangedFilesCard>,
}

/// A message this device sent that the thread has not folded yet.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PendingMessageRow {
    /// The outbox entry, for `Intent::DiscardPending`.
    pub command: CommandId,
    pub message: MessageId,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub context: Option<MessageContext>,
    pub phase: Phase,
    pub queued: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct WorkToggleRow {
    pub run: Option<RunId>,
    pub group_id: String,
    pub hidden_count: u32,
    pub expanded: bool,
    pub summary: String,
    pub icon: WorkRowIcon,
    pub tool_icon: Option<ToolIcon>,
    pub has_failure: bool,
    pub live: bool,
    pub shimmer: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum FoldKind {
    /// A settled turn's work behind "Worked for …".
    Turn,
    /// Output of an attempt a steer or restart replaced.
    Attempt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct FoldRow {
    pub kind: FoldKind,
    pub run: RunId,
    pub attempt: Option<RunAttemptId>,
    pub label: String,
    pub expanded: bool,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum TimelineRowKind {
    UserMessage(Box<UserMessageRow>),
    AssistantMessage(Box<AssistantMessageRow>),
    /// The footer of a response whose trailing tool calls follow its text.
    AssistantMeta {
        message: MessageId,
        meta: AssistantMeta,
    },
    PendingMessage(Box<PendingMessageRow>),
    /// Work-log rows; `expanded_group` lists the calls behind an open toggle.
    Work {
        rows: Vec<WorkLogRow>,
        expanded_group: bool,
    },
    /// The active turn's latest call, kept live between calls.
    LiveWork {
        label: String,
        row: WorkLogRow,
        group_id: String,
        call_count: u32,
        expanded: bool,
        active: bool,
    },
    WorkToggle(WorkToggleRow),
    Thinking {
        group_id: Option<String>,
        expanded: bool,
    },
    /// "Working for …", counted from the row's `created_at`.
    Working,
    Fold(FoldRow),
    ContextCompaction {
        label: String,
        active: bool,
    },
    Lifecycle(LifecycleRow),
    Subagents(SubagentGroupCard),
    Handoff(HandoffDivider),
    ProposedPlan(PlanCard),
    WorktreeSetup {
        snapshot: WorktreeSetupSnapshot,
        embedded: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TimelineRow {
    pub id: String,
    pub created_at: Option<Timestamp>,
    /// The next row continues the same work log.
    pub continues_work_log: bool,
    pub kind: TimelineRowKind,
}

/// What the rows of one thread derive from.
#[derive(Debug, Clone, Copy)]
pub struct TimelineSource<'a> {
    pub thread: &'a ThreadId,
    pub sync: &'a ThreadSync,
    pub shell: Option<&'a ThreadShell>,
    /// From `Outbox::undelivered_messages`.
    pub pending: &'a [PendingMessage],
    pub setup: Option<&'a WorktreeSetupSnapshot>,
    /// When this device last sent to the thread; counts until the Host names a run.
    pub send_started_at: Option<&'a Timestamp>,
    pub now_ms: i64,
}

/// A provider-native subagent thread works without app runs.
fn runless_work_started_at(state: &State) -> Option<Timestamp> {
    state.native_parent.as_ref()?;
    state
        .items
        .iter()
        .filter(|item| item.run.is_none() && !item.status.terminal())
        .map(|item| item.started_at.clone())
        .min()
}

fn stored_message(state: &State, chat: &ChatMessage) -> Message {
    state.message(&chat.id).cloned().unwrap_or_else(|| Message {
        scheduled_task: None,
        notification: None,
        id: chat.id.clone(),
        run: chat.run.clone(),
        role: chat.role,
        text: chat.text.clone(),
        attachments: chat.attachments.clone(),
        intent: chat.input_intent.unwrap_or(InputIntent::TurnStart),
        streaming: chat.streaming,
        created_by: chat.created_by.unwrap_or(MessageAuthor::User),
        creation_source: chat.creation_source.clone().unwrap_or_default(),
        created_at: chat.created_at.clone(),
        updated_at: chat.updated_at.clone(),
        context: chat.context.clone(),
    })
}

struct Context<'a> {
    state: &'a State,
    source: TimelineSource<'a>,
    options: &'a TimelineOptions,
    working: bool,
}

impl Context<'_> {
    fn detail(&self, entry_id: &str) -> Option<&Detail> {
        self.source
            .sync
            .details
            .iter()
            .find(|(item, _)| item.as_str() == entry_id)
            .map(|(_, detail)| detail)
    }

    fn work_context(&self) -> WorkRowContext<'_> {
        WorkRowContext {
            state: self.state,
            thread: Some(self.source.thread),
            workspace_root: self.options.workspace_root.as_deref(),
        }
    }

    fn pending_phase(&self, message: &MessageId) -> Option<&PendingMessage> {
        self.source.pending.iter().find(|p| &p.id == message)
    }

    fn user_row(
        &self,
        chat: &ChatMessage,
        item: Option<&Item>,
        revert_turn_count: Option<u64>,
    ) -> TimelineRowKind {
        if item.is_none()
            && let Some(pending) = self.pending_phase(&chat.id)
        {
            return pending_row(pending);
        }
        let message = stored_message(self.state, chat);
        TimelineRowKind::UserMessage(Box::new(UserMessageRow {
            message: chat.id.clone(),
            text: chat.text.clone(),
            attachments: chat.attachments.clone(),
            context: chat.context.clone(),
            decorations: user_message_decorations(
                self.state,
                item,
                &message,
                revert_turn_count,
                self.options.reverting,
                self.working,
            ),
            badge: user_message_intent_badge(message.intent),
        }))
    }

    fn changed_files(
        &self,
        run: &RunId,
        files: &[agent_domain::CheckpointFile],
    ) -> Option<ChangedFilesCard> {
        if files.is_empty() {
            return None;
        }
        let expansion = self
            .options
            .changed_files
            .get(run)
            .cloned()
            .unwrap_or_default();
        Some(changed_files_card(
            run,
            files,
            expansion.all_expanded,
            &expansion.overrides,
        ))
    }
}

fn pending_row(pending: &PendingMessage) -> TimelineRowKind {
    TimelineRowKind::PendingMessage(Box::new(PendingMessageRow {
        command: pending.command.clone(),
        message: pending.id.clone(),
        text: pending.text.clone(),
        attachments: pending.attachments.clone(),
        context: pending.context.clone(),
        phase: pending.phase.clone(),
        queued: pending.queued,
    }))
}

fn summary_icon(kind: ToolGroupSummaryKind) -> WorkRowIcon {
    match kind {
        ToolGroupSummaryKind::Action(action) => tool_group_action_icon(action),
        ToolGroupSummaryKind::DynamicTool | ToolGroupSummaryKind::Mixed => {
            WorkRowIcon::Feed(WorkIcon::Hammer)
        }
        ToolGroupSummaryKind::Reasoning => WorkRowIcon::Brain,
        ToolGroupSummaryKind::AgentTool => WorkRowIcon::Feed(WorkIcon::Agent),
        ToolGroupSummaryKind::ToneTool => WorkRowIcon::Feed(WorkIcon::Zap),
    }
}

fn desktop_rows(
    context: &Context<'_>,
    entries: Vec<super::entries::TimelineEntry>,
) -> Vec<TimelineRow> {
    let state = context.state;
    let shell = context.source.shell;
    let input = DesktopTimelineInput {
        entries,
        latest_run: shell.and_then(TimelineLatestRun::from_shell),
        running_run: shell.and_then(|shell| shell.active_run.clone()),
        expanded_runs: context.options.expanded_runs.clone(),
        expanded_attempts: context.options.expanded_attempts.clone(),
        expanded_work_groups: context.options.expanded_work_groups.clone(),
        is_working: context.working,
        runless_work_active: runless_work_started_at(state).is_some(),
        active_turn_started_at: working_started_at(shell, context.source.send_started_at)
            .or_else(|| runless_work_started_at(state)),
        turn_diffs: assistant_turn_diffs(state),
        checkpoints: state.checkpoints.clone(),
        supports_conversation_rollback: true,
        app_owned_tasks: state
            .tasks
            .iter()
            .filter(|task| task.app_owned())
            .map(|task| task.id.clone())
            .collect(),
        worktree_setup: context.source.setup.cloned(),
    };
    let work_row = |entry: &crate::view::work_log::WorkLogEntry, label: Option<&str>| {
        desktop_work_log_row(
            context.work_context(),
            entry,
            label,
            context.options.expanded_entries.contains(&entry.id),
            context.detail(&entry.id),
        )
    };
    derive_desktop_rows(&input)
        .into_iter()
        .filter_map(|row| {
            let id = row.id().to_string();
            let mut continues = false;
            let (created_at, kind) = match row {
                DesktopRow::WorktreeSetup {
                    created_at,
                    snapshot,
                    embedded,
                    ..
                } => (
                    Some(created_at),
                    TimelineRowKind::WorktreeSetup { snapshot, embedded },
                ),
                DesktopRow::Work {
                    created_at,
                    grouped_entries,
                    is_expanded_tool_group,
                    display_label,
                    continues_work_log,
                    ..
                } => {
                    continues = continues_work_log;
                    let rows = grouped_entries
                        .iter()
                        .map(|entry| work_row(entry, display_label.as_deref()))
                        .collect();
                    (
                        Some(created_at),
                        TimelineRowKind::Work {
                            rows,
                            expanded_group: is_expanded_tool_group,
                        },
                    )
                }
                DesktopRow::WorkLive {
                    created_at,
                    entry,
                    grouped_entries,
                    group_id,
                    expanded,
                    active,
                    continues_work_log,
                    ..
                } => {
                    continues = continues_work_log;
                    (
                        Some(created_at),
                        TimelineRowKind::LiveWork {
                            label: live_work_entry_label(
                                &entry,
                                context.options.workspace_root.as_deref(),
                                active,
                            ),
                            row: work_row(&entry, None),
                            group_id,
                            call_count: crate::view::count(grouped_entries.len()),
                            expanded,
                            active,
                        },
                    )
                }
                DesktopRow::Working { created_at, .. } => (created_at, TimelineRowKind::Working),
                DesktopRow::Thinking {
                    created_at,
                    group_id,
                    expanded,
                    continues_work_log,
                    ..
                } => {
                    continues = continues_work_log;
                    (created_at, TimelineRowKind::Thinking { group_id, expanded })
                }
                DesktopRow::WorkToggle {
                    created_at,
                    run,
                    group_id,
                    hidden_count,
                    expanded,
                    summary,
                    summary_kind,
                    tool_surface,
                    tool_icon,
                    summary_tool_icon,
                    has_failure,
                    continues_work_log,
                    ..
                } => {
                    continues = continues_work_log;
                    let icon = match (summary_tool_icon, tool_surface) {
                        (Some(logo), _) => WorkRowIcon::Logo(logo),
                        (None, Some(crate::view::work_log::ToolSurface::Browser)) => {
                            WorkRowIcon::Feed(WorkIcon::Browser)
                        }
                        (None, Some(crate::view::work_log::ToolSurface::Computer)) => {
                            WorkRowIcon::Feed(WorkIcon::Computer)
                        }
                        (None, None) => summary_icon(summary_kind),
                    };
                    (
                        Some(created_at),
                        TimelineRowKind::WorkToggle(WorkToggleRow {
                            run,
                            group_id,
                            hidden_count: crate::view::count(hidden_count),
                            expanded,
                            summary,
                            icon,
                            tool_icon,
                            has_failure,
                            live: false,
                            shimmer: false,
                        }),
                    )
                }
                DesktopRow::TurnFold {
                    created_at,
                    run,
                    label,
                    expanded,
                    ..
                } => (
                    Some(created_at),
                    TimelineRowKind::Fold(FoldRow {
                        kind: FoldKind::Turn,
                        run,
                        attempt: None,
                        label,
                        expanded,
                    }),
                ),
                DesktopRow::AttemptFold {
                    created_at,
                    run,
                    attempt,
                    label,
                    expanded,
                    ..
                } => (
                    Some(created_at),
                    TimelineRowKind::Fold(FoldRow {
                        kind: FoldKind::Attempt,
                        run,
                        attempt: Some(attempt),
                        label,
                        expanded,
                    }),
                ),
                DesktopRow::ContextCompaction {
                    created_at,
                    label,
                    active,
                    ..
                } => (
                    Some(created_at),
                    TimelineRowKind::ContextCompaction { label, active },
                ),
                DesktopRow::Message {
                    created_at,
                    message,
                    item,
                    duration_start,
                    show_assistant_meta,
                    show_assistant_copy_button,
                    assistant_copy_streaming,
                    assistant_turn_diff,
                    revert_turn_count,
                    ..
                } => {
                    let kind = if message.role == agent_domain::Role::User {
                        context.user_row(&message, item.as_deref(), revert_turn_count)
                    } else {
                        let stored = stored_message(state, &message);
                        TimelineRowKind::AssistantMessage(Box::new(AssistantMessageRow {
                            message: message.id.clone(),
                            text: assistant_display_text(&stored),
                            attachments: message.attachments.clone(),
                            streaming: message.streaming,
                            preserve_line_breaks: should_preserve_assistant_line_breaks(
                                &message.text,
                            ),
                            duration_start: Some(duration_start),
                            meta: show_assistant_meta.then(|| {
                                assistant_meta(
                                    item.as_deref(),
                                    &stored,
                                    show_assistant_copy_button,
                                    assistant_copy_streaming,
                                )
                            }),
                            changed_files: assistant_turn_diff
                                .and_then(|diff| context.changed_files(&diff.run, &diff.files)),
                        }))
                    };
                    (Some(created_at), kind)
                }
                DesktopRow::AssistantMeta {
                    created_at,
                    message,
                    item,
                    show_assistant_copy_button,
                    assistant_copy_streaming,
                    ..
                } => {
                    let stored = stored_message(state, &message);
                    (
                        Some(created_at),
                        TimelineRowKind::AssistantMeta {
                            message: message.id.clone(),
                            meta: assistant_meta(
                                item.as_deref(),
                                &stored,
                                show_assistant_copy_button,
                                assistant_copy_streaming,
                            ),
                        },
                    )
                }
                DesktopRow::Event {
                    created_at,
                    item,
                    subagents,
                    resource_summary,
                    ..
                } => {
                    let kind = match subagents {
                        Some(items) => TimelineRowKind::Subagents(subagent_card(context, &items)),
                        None => TimelineRowKind::Lifecycle(lifecycle_row(
                            state,
                            &item,
                            resource_summary,
                        )?),
                    };
                    (Some(created_at), kind)
                }
                DesktopRow::Handoff {
                    created_at,
                    divider,
                    ..
                } => (Some(created_at), TimelineRowKind::Handoff(divider)),
                DesktopRow::ProposedPlan {
                    created_at, plan, ..
                } => (
                    Some(created_at),
                    TimelineRowKind::ProposedPlan(plan_card(&plan)),
                ),
            };
            Some(TimelineRow {
                id,
                created_at,
                continues_work_log: continues,
                kind,
            })
        })
        .collect()
}

/// A card of the subagents of one provider turn, as the mobile feed draws them.
fn subagent_card(context: &Context<'_>, items: &[Arc<Item>]) -> SubagentGroupCard {
    let state = context.state;
    let activities: Vec<_> = items
        .iter()
        .map(|item| {
            let visibility = if state.inherited_items.iter().any(|i| i.id == item.id) {
                FeedVisibility::Inherited
            } else {
                FeedVisibility::Local
            };
            feed_activity(state, item.clone(), item.attempt.clone(), visibility)
        })
        .collect();
    let group = format!(
        "subagents:{}",
        items.first().map_or("", |item| item.id.as_str())
    );
    subagent_group_card(
        state,
        &group,
        &activities,
        context.options.expanded_work_groups.contains(&group),
        context.source.now_ms,
    )
}

fn mobile_rows(context: &Context<'_>) -> Vec<TimelineRow> {
    let state = context.state;
    let shell = context.source.shell;
    let input = FeedInput {
        latest_run: shell
            .and_then(|shell| shell.latest_run.as_ref())
            .and_then(|run| state.runs.iter().find(|candidate| &candidate.id == run))
            .map(FeedLatestRun::from),
        expanded_runs: context.options.expanded_runs.clone(),
        expanded_work_groups: context.options.expanded_work_groups.clone(),
        active_work_started_at: context
            .working
            .then(|| working_started_at(shell, context.source.send_started_at))
            .flatten()
            .or_else(|| runless_work_started_at(state)),
        runless_work_active: runless_work_started_at(state).is_some(),
        pending: context.source.pending.to_vec(),
    };
    let feed = build_thread_feed(state);
    let presented = derive_thread_feed_presentation(&feed, &input);
    append_pending_messages(presented, &feed, &input.pending)
        .into_iter()
        .map(|row| {
            let id = row.id().to_string();
            let created_at = Some(row.created_at().clone());
            let continues_work_log = row.continues_work_log();
            let kind = match row {
                FeedRow::Message { message, item, .. } => {
                    if message.role == agent_domain::Role::User {
                        context.user_row(&message, item.as_deref(), None)
                    } else {
                        let stored = stored_message(state, &message);
                        TimelineRowKind::AssistantMessage(Box::new(AssistantMessageRow {
                            message: message.id.clone(),
                            text: assistant_display_text(&stored),
                            attachments: message.attachments.clone(),
                            streaming: message.streaming,
                            preserve_line_breaks: false,
                            duration_start: None,
                            meta: (!message.streaming)
                                .then(|| assistant_meta(item.as_deref(), &stored, true, false)),
                            changed_files: None,
                        }))
                    }
                }
                FeedRow::ActivityGroup(group) => match work_log_layout(&group.activities) {
                    WorkLogLayout::Subagents => TimelineRowKind::Subagents(subagent_group_card(
                        state,
                        &group.id,
                        &group.activities,
                        context.options.expanded_work_groups.contains(&group.id),
                        context.source.now_ms,
                    )),
                    layout => TimelineRowKind::Work {
                        rows: group
                            .activities
                            .iter()
                            .map(|activity| {
                                work_log_row(
                                    state,
                                    activity,
                                    context.options.expanded_entries.contains(&activity.id),
                                    context.detail(&activity.id),
                                )
                            })
                            .collect(),
                        expanded_group: matches!(
                            layout,
                            WorkLogLayout::GroupedList | WorkLogLayout::Reasoning
                        ),
                    },
                },
                FeedRow::WorkToggle(toggle) => {
                    let presentation = work_toggle_presentation(&toggle);
                    let icon = match presentation.icon {
                        WorkToggleIcon::Logo(logo) => WorkRowIcon::Logo(logo),
                        WorkToggleIcon::Surface(crate::view::work_log::ToolSurface::Browser) => {
                            WorkRowIcon::Feed(WorkIcon::Browser)
                        }
                        WorkToggleIcon::Surface(crate::view::work_log::ToolSurface::Computer) => {
                            WorkRowIcon::Feed(WorkIcon::Computer)
                        }
                        WorkToggleIcon::Summary(kind) => summary_icon(kind),
                    };
                    TimelineRowKind::WorkToggle(WorkToggleRow {
                        run: toggle.run,
                        group_id: toggle.group_id,
                        hidden_count: crate::view::count(toggle.hidden_count),
                        expanded: toggle.expanded,
                        summary: toggle.summary,
                        icon,
                        tool_icon: toggle.tool_icon,
                        has_failure: toggle.has_failure,
                        live: toggle.live,
                        shimmer: toggle.shimmer,
                    })
                }
                FeedRow::RunFold {
                    run,
                    label,
                    expanded,
                    ..
                } => TimelineRowKind::Fold(FoldRow {
                    kind: FoldKind::Turn,
                    run,
                    attempt: None,
                    label,
                    expanded,
                }),
                FeedRow::Thinking { .. } => TimelineRowKind::Thinking {
                    group_id: None,
                    expanded: false,
                },
                FeedRow::Handoff { divider, .. } => TimelineRowKind::Handoff(divider),
                FeedRow::PendingMessage(pending) => pending_row(&pending),
            };
            TimelineRow {
                id,
                created_at,
                continues_work_log,
                kind,
            }
        })
        .collect()
}

/// Whether the thread has work in flight: a run starting or running, a send
/// this device has not had confirmed, or a native subagent's runless turn.
fn is_working(state: &State, source: &TimelineSource<'_>) -> bool {
    let run_working = source.shell.is_some_and(|shell| {
        shell
            .activity_run_status
            .is_some_and(|status| !status.terminal())
    });
    let sending = source
        .pending
        .iter()
        .any(|pending| !pending.queued && pending.phase != Phase::Queued);
    run_working || sending || runless_work_started_at(state).is_some()
}

/// The rows of a thread in the chosen layout; empty before its state arrives.
pub fn timeline_rows(source: TimelineSource<'_>, options: &TimelineOptions) -> Vec<TimelineRow> {
    let Some(state) = source.sync.state.as_deref() else {
        return source
            .pending
            .iter()
            .map(|pending| TimelineRow {
                id: pending.id.to_string(),
                created_at: Some(pending.created_at.clone()),
                continues_work_log: false,
                kind: pending_row(pending),
            })
            .collect();
    };
    let context = Context {
        state,
        source,
        options,
        working: is_working(state, &source),
    };
    match options.layout {
        TimelineLayout::Desktop => {
            let entries = derive_timeline_entries(
                state,
                &EntriesInput {
                    pending: source.pending,
                },
            );
            desktop_rows(&context, entries)
        }
        TimelineLayout::Mobile => mobile_rows(&context),
    }
}

/// Shared timeline storage; native clients read values only after its revision changes.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct ThreadRows(Vec<TimelineRow>);
impl std::ops::Deref for ThreadRows {
    type Target = Vec<TimelineRow>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl ThreadRows {
    pub fn values(&self) -> Vec<TimelineRow> {
        self.0.clone()
    }
}

/// The rows of one thread with a revision that advances when they change.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadTimeline {
    pub rows: Arc<ThreadRows>,
    pub revision: u64,
}

/// How to bring a list showing `previous` rows to the current ones.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TimelineUpdate {
    pub splice: Splice,
    /// Indexes of kept rows whose content changed, to measure again.
    pub changed: Vec<u32>,
}

pub fn timeline_update(previous: &[TimelineRow], next: &[TimelineRow]) -> TimelineUpdate {
    TimelineUpdate {
        splice: splice(previous, next, |row| &row.id),
        changed: changed(previous, next, |row| &row.id)
            .into_iter()
            .map(crate::view::count)
            .collect(),
    }
}

#[derive(Debug, Clone, PartialEq)]
struct TimelineKey {
    cursor: u64,
    history_revision: u64,
    detail_revision: u64,
    shell: Option<ThreadShell>,
    pending: Vec<PendingMessage>,
    setup: Option<u64>,
    send_started_at: Option<Timestamp>,
    options: TimelineOptions,
    /// The second, only while live subagent cards count elapsed time.
    clock: Option<i64>,
}

impl TimelineKey {
    fn of(source: &TimelineSource<'_>, options: &TimelineOptions) -> Self {
        let live_subagents = source.sync.state.as_deref().is_some_and(|state| {
            state.items.iter().any(|item| {
                matches!(item.kind, ItemKind::Subagent { .. }) && !item.status.terminal()
            })
        });
        Self {
            cursor: source.sync.cursor,
            history_revision: source.sync.history_revision,
            detail_revision: source.sync.detail_revision,
            shell: source.shell.cloned(),
            pending: source.pending.to_vec(),
            setup: source.setup.map(|setup| setup.sequence),
            send_started_at: source.send_started_at.cloned(),
            options: options.clone(),
            clock: live_subagents.then_some(source.now_ms / 1_000),
        }
    }
}

/// The last rows of each thread, rebuilt only when their inputs change.
#[derive(Debug, Default)]
pub struct TimelineCache {
    threads: HashMap<ThreadId, (TimelineKey, ThreadTimeline)>,
}

impl TimelineCache {
    pub fn timeline(
        &mut self,
        source: TimelineSource<'_>,
        options: &TimelineOptions,
    ) -> ThreadTimeline {
        let key = TimelineKey::of(&source, options);
        if let Some((cached, timeline)) = self.threads.get(source.thread)
            && cached == &key
        {
            return timeline.clone();
        }
        let rows = timeline_rows(source, options);
        let revision = match self.threads.get(source.thread) {
            Some((_, previous)) if previous.rows.as_slice() == rows.as_slice() => {
                let timeline = previous.clone();
                self.threads
                    .insert(source.thread.clone(), (key, timeline.clone()));
                return timeline;
            }
            Some((_, previous)) => previous.revision + 1,
            None => 1,
        };
        let timeline = ThreadTimeline {
            rows: Arc::new(ThreadRows(rows)),
            revision,
        };
        self.threads
            .insert(source.thread.clone(), (key, timeline.clone()));
        timeline
    }

    /// Drops the rows of threads no longer shown.
    pub fn retain(&mut self, keep: impl Fn(&ThreadId) -> bool) {
        self.threads.retain(|thread, _| keep(thread));
    }
}

#[cfg(test)]
#[path = "rows_tests.rs"]
mod tests;
