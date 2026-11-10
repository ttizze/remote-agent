use crate::l10n::{tr, tr_format};
use crate::ui::Enabled;
use crate::{Connection, shared::Shared};
use agent_core::{
    presentation::conversation::{
        ActivityPresentation, ConversationRowContent, LANE_WIDTH, RenderedConversation,
        RenderedTurn, TurnErrorPresentation, TurnRow, WorkProgress, lane_padding, turn_rows,
    },
    state::{Intent, operations as op},
};
use nsh_core::geometry::Points;
use nsh_core::prelude::*;
use std::collections::{HashMap, HashSet};

/// T3 Code's timeline colors, in its light and dark themes.
pub(crate) mod look {
    use crate::ui::Tone;
    use nsh_core::element::rgb;

    /// `--muted-foreground` and `--secondary-label`: work rows, headers, times.
    pub(crate) const MUTED: Tone = Tone::new(rgb(0x71717a), rgb(0x888888));
    /// `--icon-muted`: a tool's icon.
    pub(crate) const ICON: Tone = Tone::new(rgb(0xa1a1aa), rgb(0x6a6a6a));
    /// `--foreground`: the shimmer's band, a hovered header.
    pub(crate) const FOREGROUND: Tone = Tone::new(rgb(0x18181b), rgb(0xe6e6e6));
    /// `bg-message`: the user's bubble.
    pub(crate) const BUBBLE: Tone = Tone::new(rgb(0xebebed), rgb(0x141414));
    /// `border-border/60`: the rule under a turn's header.
    pub(crate) const RULE: Tone = Tone::new(rgb(0xeeeef0), rgb(0x161616));
    /// `text-foreground/80`: an error's message.
    pub(crate) const MESSAGE: Tone = Tone::new(rgb(0x454548), rgb(0xc4c4c4));
    /// `bg-input/32`: a code block.
    pub(crate) const CODE: Tone = Tone::new(rgb(0xefeff1), rgb(0x111111));
    /// `--destructive`.
    pub(crate) const DESTRUCTIVE: Tone = Tone::new(rgb(0xfb2c36), rgb(0xfb414a));
    /// `border-border/80` around a card (a proposed plan) and an avatar.
    pub(crate) const CARD_BORDER: Tone = Tone::new(rgb(0xe9e9ec), rgb(0x1b1b1b));
    /// `--secondary` and an outlined control's fill: a badge, a menu button.
    pub(crate) const CONTROL: Tone = Tone::new(rgb(0xfafafa), rgb(0x161616));
    /// A subagent's avatar.
    pub(crate) const AVATAR: Tone = Tone::new(rgb(0xf4f4f5), rgb(0x121212));
    /// `--success`: a subagent that finished.
    pub(crate) const SUCCESS: Tone = Tone::new(rgb(0x00bc7d), rgb(0x01b276));
    /// `text-sm leading-relaxed`, in rows that are not Markdown.
    pub(crate) const TEXT: f32 = 14.0;
    pub(crate) const LINE: f32 = 22.75;
    /// A work log row's icon box (`size-6`), its icon (`size-4`) and the gap
    /// to its label (`gap-1.5`).
    pub(crate) const ICON_BOX: f32 = 24.0;
    pub(crate) const ICON_SIZE: f32 = 16.0;
    pub(crate) const ICON_GAP: f32 = 6.0;
}

pub(crate) fn reveal_row(list: ElementRef, index: u32) {
    after_answer(move || {
        list.scroll_to_index(index, nsh_core::geometry::Percentage(0.0), px(0.0), false);
    });
}

/// Whose a row is.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Kind {
    User,
    Response,
    Activity,
}

/// What a row draws, as T3 Code's timeline rows.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Shape {
    /// A message: the user's, an answer, or text a client shows as it is.
    Message,
    /// Commentary between the work, without an answer's actions.
    Commentary,
    /// A completed turn's "Worked for …".
    Fold { open: bool },
    /// A running turn's "Working for …".
    Working,
    /// Work under its summary.
    Group { open: bool },
    /// A tool's row; a `member` of an open group sits tight in it.
    Work { member: bool },
    /// The running turn's latest work, Thinking, a retry.
    Live,
    /// The turn's error.
    Error,
    /// A plan the agent proposed: its card.
    Plan,
    /// A subagent the turn started: its row.
    Subagent,
    /// A divider across the work (context compacted).
    Divider,
    /// The settled turn's answer's actions, below the work that ends it.
    Actions,
    /// The turn was interrupted.
    Interrupted,
}

impl Shape {
    /// One of T3 Code's work log rows, which run on without space between.
    fn work_log(self) -> bool {
        matches!(
            self,
            Self::Group { .. } | Self::Work { .. } | Self::Live | Self::Error
        )
    }
}

#[derive(Clone, PartialEq)]
struct Row {
    id: String,
    body: SharedString,
    kind: Kind,
    shape: Shape,
    /// A group's work, which its icon names (`Icon::work`).
    group_kind: String,
    /// The space below it, as T3 Code's rows keep.
    gap: f32,
    actions: bool,
    activity: Option<ActivityPresentation>,
    fork: Option<agent_protocol::ids::TurnId>,
    retry: Option<agent_protocol::operations::RetryPreparation>,
    images: Vec<String>,
    detail: Option<crate::activity::Item>,
    /// A live status line in place of its body: thinking, setting up.
    status: Option<crate::activity::Status>,
    progress: Option<WorkProgress>,
    /// The local time it was sent or answered.
    time: Option<String>,
    /// A message sent while its turn worked; only the first of a run says so.
    steer: bool,
    /// The message waited in the queue, then started its turn.
    queued: bool,
    /// The turn "Edit from here" returns to just before.
    edit_turn: Option<agent_protocol::ids::TurnId>,
    /// A plan's complete Markdown.
    note: SharedString,
    /// A notification row's icon.
    notification: Option<crate::ui::Icon>,
    agent: Option<agent_core::presentation::agents::SubagentPresentation>,
    error: Option<TurnErrorPresentation>,
    /// The message's id while the Host may not have received it: it goes back
    /// to the composer or is discarded, as the desktop client's controls do.
    unresolved: Option<String>,
}

impl Row {
    fn new(id: String, kind: Kind, shape: Shape, body: impl Into<SharedString>) -> Self {
        Self {
            id,
            body: body.into(),
            kind,
            shape,
            group_kind: String::new(),
            gap: 0.0,
            actions: false,
            activity: None,
            fork: None,
            edit_turn: None,
            retry: None,
            images: Vec::new(),
            detail: None,
            status: None,
            progress: None,
            time: None,
            steer: false,
            queued: false,
            notification: None,
            agent: None,
            note: SharedString::default(),
            error: None,
            unresolved: None,
        }
    }
}

fn project(
    conversation: &RenderedConversation,
    expanded: &HashMap<String, String>,
    groups: &HashSet<String>,
) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    for turn in &conversation.turns {
        let key = turn.key();
        for row in turn_rows(turn, expanded, groups) {
            rows.extend(turn_row(row, &key, turn));
        }
    }
    rows.extend(conversation.queued.iter().filter_map(|item| {
        source_row(agent_core::presentation::conversation::ConversationRow {
            id: item.data.id.clone(),
            content: ConversationRowContent::User { item: item.clone() },
            timestamp_ms: item.data.timestamp_ms,
            steer: false,
            queued: false,
            edit_turn_id: None,
        })
    }));
    rows.extend(
        conversation
            .session_rows
            .iter()
            .cloned()
            .filter_map(source_row),
    );
    let mut previous_steer = false;
    let mut in_group = false;
    for row in &mut rows {
        let steer = row.steer;
        row.steer = steer && !previous_steer;
        previous_steer = steer;
        // An open group's tools sit in it.
        if let Shape::Work { member } = &mut row.shape {
            *member = in_group;
        }
        in_group = matches!(
            row.shape,
            Shape::Group { open: true } | Shape::Work { member: true }
        );
    }
    let shapes: Vec<_> = rows.iter().map(|row| row.shape).collect();
    for (index, row) in rows.iter_mut().enumerate() {
        row.gap = gap(
            row.kind,
            row.shape,
            row.actions,
            shapes.get(index + 1).copied(),
        );
    }
    rows
}

/// The space below a row, as T3 Code's timeline rows keep: a message's
/// `pb-4` (`pb-2` without its actions), the work headers' `pb-1.5`, and
/// none between work log rows, which run on as one list.
fn gap(kind: Kind, shape: Shape, actions: bool, next: Option<Shape>) -> f32 {
    let continues = next.is_some_and(Shape::work_log);
    match shape {
        Shape::Fold { .. } | Shape::Working => 6.0,
        Shape::Group { open: true } => 0.0,
        Shape::Work { member: true } if next == Some(Shape::Work { member: true }) => 0.0,
        Shape::Work { member: true } if !continues => 4.0,
        shape if shape.work_log() => {
            if continues {
                0.0
            } else {
                8.0
            }
        }
        Shape::Commentary | Shape::Interrupted => 8.0,
        Shape::Subagent => 0.0,
        Shape::Message if kind == Kind::Response && !actions => 8.0,
        _ => 16.0,
    }
}

/// A row of a turn on T3 Code's timeline.
fn turn_row(row: TurnRow, key: &str, turn: &RenderedTurn) -> Option<Row> {
    use crate::activity::{Item, Status};
    let activity_item =
        |row: &agent_core::presentation::conversation::ConversationRow, live, label| {
            let ConversationRowContent::Activity { item, turn_id } = &row.content else {
                return None;
            };
            Some(Item {
                source: Shared(item.clone()),
                turn: turn_id.clone(),
                live,
                label,
            })
        };
    let status = |id: &str, status| {
        let mut row = Row::new(format!("{id}:{key}"), Kind::Activity, Shape::Live, "");
        row.status = Some(status);
        row
    };
    Some(match row {
        TurnRow::Row(row) => source_row(row)?,
        TurnRow::Fold { activity, open } => {
            let mut row = Row::new(
                activity.id.clone(),
                Kind::Activity,
                Shape::Fold { open },
                activity.activity_summary.clone(),
            );
            row.activity = Some(activity);
            row
        }
        TurnRow::Working => {
            let mut row = Row::new(format!("working:{key}"), Kind::Activity, Shape::Working, "");
            row.progress = Some(turn.work_progress(false));
            row
        }
        TurnRow::Commentary(item) => {
            let mut row = Row::new(
                format!("commentary:{}", item.data.id),
                Kind::Response,
                Shape::Commentary,
                item.data.body.clone().unwrap_or_default(),
            );
            row.images = item.data.image_sources.clone();
            row
        }
        TurnRow::Group {
            id,
            summary,
            kind,
            open,
        } => {
            let mut row = Row::new(id, Kind::Activity, Shape::Group { open }, summary);
            row.group_kind = kind;
            row
        }
        TurnRow::Work { row, label } => {
            let mut shown = Row::new(
                row.id.clone(),
                Kind::Activity,
                Shape::Work { member: false },
                label.clone(),
            );
            shown.detail = activity_item(&row, false, label);
            shown
        }
        TurnRow::Live { row, label } => {
            let mut shown = Row::new(row.id.clone(), Kind::Activity, Shape::Live, label.clone());
            shown.detail = activity_item(&row, true, label);
            shown
        }
        TurnRow::Thinking => status("thinking", Status::Thinking),
        TurnRow::Retrying { label } => {
            let mut row = status("retrying", Status::Retrying);
            row.body = label.into();
            row
        }
        TurnRow::Requested { label, approval } => {
            let mut row = status("requested", Status::Requested { approval });
            row.body = label.into();
            row
        }
        TurnRow::Actions(row) => {
            // The answer's actions alone: not an answer to show or name again.
            let mut shown = source_row(row)?;
            shown.id = format!("actions:{}", shown.id);
            shown.kind = Kind::Activity;
            shown.shape = Shape::Actions;
            shown.images.clear();
            shown
        }
        TurnRow::Plan { id, title, text } => {
            let mut row = Row::new(format!("plan:{id}"), Kind::Activity, Shape::Plan, title);
            row.note = text.into();
            row
        }
        TurnRow::Subagent { id, agent } => {
            let mut row = Row::new(
                format!("subagent:{id}"),
                Kind::Activity,
                Shape::Subagent,
                agent.name.clone(),
            );
            row.agent = Some(agent);
            row
        }
        TurnRow::Divider { id, label } => Row::new(
            format!("divider:{id}"),
            Kind::Activity,
            Shape::Divider,
            label,
        ),
        TurnRow::Interrupted => Row::new(
            format!("interrupted:{key}"),
            Kind::Activity,
            Shape::Interrupted,
            tr("実行を中断しました"),
        ),
    })
}

/// A row of the conversation's projection as it is.
fn source_row(row: agent_core::presentation::conversation::ConversationRow) -> Option<Row> {
    let (kind, shape, body) = match &row.content {
        ConversationRowContent::User { item } => (
            Kind::User,
            Shape::Message,
            item.data.body.clone().unwrap_or_default(),
        ),
        ConversationRowContent::Response { item, .. } => (
            Kind::Response,
            Shape::Message,
            item.data.body.clone().unwrap_or_default(),
        ),
        ConversationRowContent::Activity { item, .. } => (
            Kind::Activity,
            Shape::Message,
            item.data
                .body
                .clone()
                .or_else(|| item.data.title.clone())
                .unwrap_or_default(),
        ),
        ConversationRowContent::Preparation { preparation } => {
            (Kind::Activity, Shape::Message, preparation.text.clone())
        }
        ConversationRowContent::ProviderHandoff { text } => {
            (Kind::Activity, Shape::Message, text.clone())
        }
        ConversationRowContent::Notification { reports, .. } => (
            Kind::Activity,
            Shape::Message,
            agent_core::presentation::background_work::notification_text(
                reports.clone(),
                crate::l10n::language(),
            ),
        ),
        ConversationRowContent::Error { error } => {
            (Kind::Activity, Shape::Error, error.message.clone())
        }
        ConversationRowContent::ActivityHeader { .. }
        | ConversationRowContent::InProgress { .. }
        | ConversationRowContent::PendingRequest { .. } => return None,
    };
    let mut shown = Row::new(row.id, kind, shape, body);
    (shown.steer, shown.queued) = (row.steer, row.queued);
    shown.edit_turn = row.edit_turn_id;
    shown.time = row
        .timestamp_ms
        .and_then(|ms| i64::try_from(ms).ok())
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|time| {
            use agent_core::presentation::conversation::{CalendarDay, day_aware_time};
            use chrono::{Datelike, Timelike};
            let time = time.with_timezone(&chrono::Local);
            let today = chrono::Local::now().date_naive();
            day_aware_time(
                crate::l10n::language(),
                time.hour(),
                time.minute(),
                CalendarDay {
                    year: time.year(),
                    month: time.month(),
                    day: time.day(),
                },
                (today - time.date_naive()).num_days(),
                today.year(),
            )
        });
    match row.content {
        ConversationRowContent::User { item } => {
            shown.images = item.data.image_sources.clone();
            shown.unresolved = item.unknown_submission_id();
        }
        ConversationRowContent::Response {
            item,
            show_actions,
            fork_turn_id,
        } => {
            shown.images = item.data.image_sources.clone();
            shown.actions = show_actions;
            shown.fork = fork_turn_id;
        }
        ConversationRowContent::Activity { item, .. } => {
            shown.images = item.data.image_sources.clone();
        }
        ConversationRowContent::Preparation { preparation } => {
            shown.retry = preparation.retry;
            if preparation.shimmer {
                shown.status = Some(crate::activity::Status::SettingUp);
            }
        }
        ConversationRowContent::Notification { icon, .. } => {
            shown.notification = Some(crate::ui::Icon::notification(icon));
        }
        ConversationRowContent::Error { error } => shown.error = Some(error),
        _ => {}
    }
    Some(shown)
}
/// The background work the agent woke for, as T3 Code's notification row:
/// the work's icon beside what it did.
fn notification_row(row: Memo<Option<Row>>) -> impl IntoElement {
    use crate::ui::{self, colors};
    let icon = row.with_untracked(|row| row.as_ref().and_then(|row| row.notification));
    view()
        .flex_row()
        .items_center()
        .gap_2()
        .text_color(colors::MUTED)
        .child(icon.map(|icon| ui::icon(icon, 14.0).into_element()))
        .child(
            text(move || {
                row.with(|row| row.as_ref().map(|row| row.body.clone()).unwrap_or_default())
            })
            .text_size(px(13.0)),
        )
}

/// A conversation's timeline: its rows and the reader's place in them, which
/// its list and, beside the list and the composer, its minimap share.
#[derive(Clone, Copy)]
pub(crate) struct Timeline {
    following: Signal<bool>,
    list: ElementRef,
    expanded: Signal<HashMap<String, String>>,
    /// The work groups opened.
    groups: Signal<HashSet<String>>,
    rows: Memo<Shared<Vec<Row>>>,
    /// The first and last rows in view.
    viewable: Signal<Option<(usize, usize)>>,
    /// The timeline's width, its lane centred in it.
    width: Signal<f32>,
    /// The space the timeline's end keeps clear of what lies over it (the
    /// composer): as T3 Code's reservation, the tallest it was in this
    /// conversation, so that it shrinking never pulls a reader to the end.
    inset: Signal<f32>,
    /// The composer's current height positions the latest-message button and
    /// starts the next conversation's scroll reservation.
    covered: Signal<f32>,
}

impl Timeline {
    pub(crate) fn new(following: Signal<bool>) -> Self {
        let connection = use_context::<Connection>().unwrap();
        let expanded = signal(HashMap::<String, String>::new());
        let groups = signal(HashSet::<String>::new());
        // Shared, so that a row reads it cheaply through `try_get`.
        let rows = computed(move || {
            let projected = connection.conversation.with(|conversation| {
                conversation
                    .as_ref()
                    .map(|conversation| {
                        expanded.with(|expanded| {
                            groups.with(|groups| project(conversation, expanded, groups))
                        })
                    })
                    .unwrap_or_default()
            });
            #[cfg(all(target_os = "macos", debug_assertions))]
            crate::acceptance::rows_ready(projected.len());
            Shared(std::sync::Arc::new(projected))
        });
        Self {
            following,
            list: element_ref(),
            expanded,
            groups,
            rows,
            viewable: signal(None),
            width: signal(0.0),
            inset: signal(0.0),
            covered: signal(0.0),
        }
    }

    /// As T3 Code's composer overlay: what lies over the timeline's end,
    /// `height` tall, which the timeline's end and its way back clear.
    pub(crate) fn cover(self, height: f32) {
        if self.covered.get_untracked() != height {
            self.covered.set(height);
        }
        if self.inset.get_untracked() < height {
            self.inset.set(height);
        }
    }

    /// The lane's width, which message text lays out to.
    fn lane(self) -> f32 {
        (self.width.get() - 2.0 * lane_padding(window().width.0)).clamp(0.0, LANE_WIDTH)
    }

    pub(crate) fn list(self) -> impl IntoElement {
        list(self)
    }

    /// T3 Code's minimap at the leading edge of the conversation, beside its
    /// timeline and composer, on a canvas `canvas` points wide.
    pub(crate) fn minimap(self, canvas: impl Fn() -> f32 + Copy + 'static) -> impl IntoElement {
        when(is_desktop, move || minimap(self, canvas))
    }
}

fn list(timeline: Timeline) -> impl IntoElement {
    let Timeline {
        following,
        list,
        expanded,
        groups,
        rows,
        viewable,
        width,
        inset,
        covered,
    } = timeline;
    let surroundings = crate::rich_text::Surroundings::current();
    let connection = surroundings.connection;
    let saver = crate::plans::Saver::new();
    let expanded_items = signal(HashSet::<String>::new());
    // Where the list was last scrolled to, for Core's follow rule.
    let scrolled = stored(None::<agent_core::presentation::conversation::ScrollPosition>);
    // Further from the end than following reaches, as T3 Code offers the way
    // back.
    let away_from_end = signal(false);
    let clock = signal(std::time::SystemTime::now());
    let padding = move || lane_padding(window().width.0);
    // Reused cells must not keep a width measured for another row.
    let column_width = computed(move || timeline.lane());
    spawn(async move {
        loop {
            timer(std::time::Duration::from_secs(1)).await;
            let running = connection
                .composer
                .with_untracked(|p| p.controls.running_turn.is_some())
                || connection.publication.with_untracked(|p| {
                    rows.with_untracked(|rows| {
                        rows.iter()
                            .filter_map(|row| row.agent.as_ref())
                            .any(|agent| {
                                let child = agent.target.as_ref().and_then(|target| {
                                    p.snapshot.sessions.conversations.get(target)
                                });
                                agent.status(child.map(|child| &**child))
                                    == agent_protocol::models::TurnStatus::Running
                            })
                    })
                });
            if running {
                clock.set(std::time::SystemTime::now());
            }
        }
    });
    // A signal, not a memo: the rows read it while the conversation changes
    // under them, and a signal always holds the last conversation it named.
    let current = move || {
        (
            connection.host.get(),
            connection
                .composer
                .with(|p| p.controls.navigation.draft_key.clone()),
        )
    };
    let target = signal(untracked(current));
    effect(move || {
        let now = current();
        if target.with_untracked(|target| *target != now) {
            target.set(now);
        }
    });
    effect(move || {
        target.get();
        following.set(true);
        scrolled.replace(None);
        expanded.set(HashMap::new());
        groups.set(HashSet::new());
        expanded_items.set(HashSet::new());
        // A reservation held for another conversation's draft is not this one's.
        inset.set(covered.get_untracked());
    });
    // T3 Code's thread error banner over the timeline's top.
    let banner = computed(move || connection.context.with(|p| p.thread_error().is_some()));
    // A conversation still opening says why itself, retry included.
    let history = computed(move || {
        connection
            .context
            .with(|p| p.opening().is_none().then(|| p.history()).flatten())
    });
    let column = view().label(tr("会話の履歴"));
    #[cfg(desktop)]
    let column = column.child(surroundings.selection_bar());
    column
        .flex_1()
        .min_h_0()
        .on_layout(move |bounds: &Bounds<Points>| width.set(bounds.size.width.0))
        .child(saver.view(connection))
        .child(opening(connection))
        .child(when(
            move || history.get().is_some(),
            move || {
                view()
                    .gap_2()
                    .items_center()
                    .child(
                        crate::ui::text_button(
                            move || {
                                SharedString::from(history.with(|history| {
                                    history
                                        .as_ref()
                                        .map(|history| history.load_label.clone())
                                        .unwrap_or_default()
                                }))
                            },
                            move || {
                                following.set(false);
                                if let Some(thread_id) = connection
                                    .context
                                    .with_untracked(|p| p.navigation().thread_id)
                                {
                                    connection.dispatch(Intent::ReadOlder { thread_id });
                                }
                            },
                        )
                        .enabled(move || {
                            history.with(|history| {
                                history.as_ref().is_some_and(|history| !history.loading)
                            })
                        }),
                    )
                    .child(text(move || {
                        history.with(|history| {
                            history
                                .as_ref()
                                .and_then(|history| history.error.as_ref())
                                .map(|error| error.to_string())
                                .unwrap_or_default()
                        })
                    }))
            },
        ))
        .child(text(move || {
            connection.conversation.with(|conversation| {
                conversation
                    .as_ref()
                    .and_then(|conversation| conversation.history_notice())
                    .unwrap_or_default()
            })
        }))
        // Cell measurements and native text views belong to this conversation.
        // Reusing the list across tasks carries their heights and scroll offset
        // into an unrelated history before its native measurements arrive.
        .child(each(
            move || {
                (column_width.get() > 0.0 && rows.with(|rows| !rows.is_empty()))
                    .then(|| target.get())
                    .into_iter()
                    .collect::<Vec<_>>()
            },
            |target| target.clone(),
            move |_, _| {
                virtual_list(
                    move || rows.with(|rows| rows.len()),
                    move |at| {
                        let binding = stored((None::<(usize, String)>, None::<usize>));
                        // Asks again once the runtime answered a retention.
                        let recheck = signal(0_u32);
                        let row = computed(move || {
                            // A splice updates the data before the recycled slot's
                            // Position. Resolve its retained key in this snapshot
                            // so a live answer never briefly becomes another row.
                            recheck.get();
                            let rows = rows.try_get()?;
                            let snapshot = std::sync::Arc::as_ptr(&rows.0) as usize;
                            let position = at.index();
                            let (previous, retained_in) = binding.get();
                            let retained = previous
                                .as_ref()
                                .and_then(|(_, id)| rows.iter().position(|row| &row.id == id));
                            let index = agent_core::presentation::conversation::retained_row_index(
                                previous.as_ref().map(|(position, _)| *position),
                                position,
                                retained,
                                rows.len(),
                                retained_in == Some(snapshot),
                            );
                            let retaining = index.is_some_and(|index| index != position);
                            if retaining {
                                after_answer(move || {
                                    // The cell may have left the list.
                                    if let Some(count) = recheck.try_get() {
                                        recheck.try_set(count.wrapping_add(1));
                                    }
                                });
                            }
                            let row = index.and_then(|index| rows.get(index)).cloned();
                            binding.replace((
                                row.as_ref().map(|row| (position, row.id.clone())),
                                retaining.then_some(snapshot),
                            ));
                            row
                        });
                        let user =
                            move || row.with(|r| r.as_ref().is_some_and(|r| r.kind == Kind::User));
                        let kind =
                            move || row.with(|r| r.as_ref().map_or(Kind::Response, |r| r.kind));
                        let detail = computed(move || {
                            row.with(|r| r.as_ref().and_then(|r| r.detail.clone()))
                        });
                        let error = computed(move || {
                            row.with(|r| r.as_ref().and_then(|r| r.error.clone()))
                        });
                        let status =
                            computed(move || row.with(|r| r.as_ref().and_then(|r| r.status)));
                        let label = move || {
                            row.with(|row| {
                                row.as_ref().map(|row| {
                                    row.progress
                                        .as_ref()
                                        .map(|progress| {
                                            progress
                                                .label(
                                                    crate::l10n::language(),
                                                    // A cell can outlive the list's clock
                                                    // while its conversation closes.
                                                    clock
                                                        .try_get()
                                                        .unwrap_or_else(std::time::SystemTime::now)
                                                        .duration_since(std::time::UNIX_EPOCH)
                                                        .unwrap_or_default()
                                                        .as_secs_f64(),
                                                )
                                                .into()
                                        })
                                        .unwrap_or_else(|| row.body.clone())
                                })
                            })
                        };
                        let shape = move || row.with(|r| r.as_ref().map(|r| r.shape));
                        let message = move || {
                            row.with(|row| {
                                row.as_ref().is_some_and(|row| {
                                    matches!(row.shape, Shape::Message | Shape::Commentary)
                                        && row.status.is_none()
                                        && row.notification.is_none()
                                })
                            })
                        };
                        let time = move || {
                            row.with(|row| row.as_ref().and_then(|row| row.time.clone()))
                                .unwrap_or_default()
                        };
                        view()
                            .w_full()
                            .items_center()
                            .px(move || px(padding()))
                            .pt(move || {
                                // T3 Code's spacer above the first row: the
                                // top bar's fade, or less under the error
                                // banner, which takes the fade's place.
                                // A cell can outlive the list while its
                                // conversation closes.
                                px(match at.index() {
                                    0 if banner.try_get().unwrap_or(false) => 16.0,
                                    0 => 24.0,
                                    _ => 0.0,
                                })
                            })
                            .pb(move || {
                                // The last row ends clear of what lies over the
                                // end, and T3 Code's footer (`sm:h-4`) below it.
                                let last = at.index() + 1 == rows.with(|rows| rows.len());
                                px(row.with(|row| row.as_ref().map_or(0.0, |row| row.gap))
                                    + if last { inset.get() + 16.0 } else { 0.0 })
                            })
                            .child(
                                view()
                                    .group()
                                    .w_full()
                                    .max_w(px(LANE_WIDTH))
                                    .items_start()
                                    .style_when(user, |s| s.items_end())
                                    .child(when(
                                        move || {
                                            row.with(|row| {
                                                row.as_ref().is_some_and(|row| row.steer)
                                            })
                                        },
                                        steer_marker,
                                    ))
                                    .child(when(
                                        move || {
                                            row.with(|row| {
                                                row.as_ref().is_some_and(|row| row.queued)
                                            })
                                        },
                                        queued_marker,
                                    ))
                                    .child(when(
                                        move || {
                                            row.with(|row| {
                                                row.as_ref()
                                                    .is_some_and(|row| row.notification.is_some())
                                            })
                                        },
                                        move || notification_row(row),
                                    ))
                                    .child(when(
                                        move || matches!(shape(), Some(Shape::Fold { .. })),
                                        move || {
                                            let open =
                                                move || shape() == Some(Shape::Fold { open: true });
                                            view()
                                                .w_full()
                                                .child(fold_row(
                                                    move || label().unwrap_or_default().to_string(),
                                                    open,
                                                    time,
                                                    move || {
                                                        if let Some(activity) =
                                                            row.with_untracked(|row| {
                                                                row.as_ref().and_then(|row| {
                                                                    row.activity.clone()
                                                                })
                                                            })
                                                        {
                                                            // As T3 Code's, the reader stays
                                                            // where they were: at the end if
                                                            // they followed it.
                                                            let opening = !untracked(open);
                                                            expanded.update(|all| {
                                                                if opening {
                                                                    all.insert(
                                                                        activity.id.clone(),
                                                                        activity.status,
                                                                    );
                                                                } else {
                                                                    all.remove(&activity.id);
                                                                }
                                                            });
                                                            if opening
                                                                && let Some(load) =
                                                                    activity.load_items
                                                            {
                                                                connection.dispatch(
                                                                    Intent::LoadTurnItems(load),
                                                                );
                                                            }
                                                        }
                                                    },
                                                ))
                                                .child(work_load_failure(connection, row, open))
                                        },
                                    ))
                                    .child(when(
                                        move || shape() == Some(Shape::Working),
                                        move || {
                                            working_row(move || {
                                                label().unwrap_or_default().to_string()
                                            })
                                        },
                                    ))
                                    .child(when(
                                        move || matches!(shape(), Some(Shape::Group { .. })),
                                        move || {
                                            group_row(row, time, move || {
                                                let Some(id) = row.with_untracked(|row| {
                                                    row.as_ref().map(|row| row.id.clone())
                                                }) else {
                                                    return;
                                                };
                                                groups.update(|groups| {
                                                    if !groups.remove(&id) {
                                                        groups.insert(id);
                                                    }
                                                });
                                            })
                                        },
                                    ))
                                    .child(when(
                                        move || detail.get().is_some(),
                                        move || {
                                            // T3 Code's work log row: `py-0.5`,
                                            // none in an open group's list.
                                            view()
                                                .w_full()
                                                .py(move || {
                                                    let member = matches!(
                                                        shape(),
                                                        Some(Shape::Work { member: true })
                                                    );
                                                    px(if member { 0.0 } else { 2.0 })
                                                })
                                                .child(crate::activity::details(
                                                    surroundings,
                                                    detail,
                                                    expanded_items,
                                                    column_width,
                                                ))
                                        },
                                    ))
                                    .child(when(
                                        move || status.get().is_some(),
                                        move || {
                                            crate::activity::status(status, move || {
                                                label().unwrap_or_default().to_string()
                                            })
                                        },
                                    ))
                                    .child(when(
                                        move || shape() == Some(Shape::Plan),
                                        move || plan_card(surroundings, row, column_width, saver),
                                    ))
                                    .child(when(
                                        move || shape() == Some(Shape::Subagent),
                                        move || subagent_row(connection, row, clock),
                                    ))
                                    .child(when(
                                        move || shape() == Some(Shape::Divider),
                                        move || {
                                            divider_row(move || {
                                                label().unwrap_or_default().to_string()
                                            })
                                        },
                                    ))
                                    .child(when(
                                        move || shape() == Some(Shape::Interrupted),
                                        move || {
                                            interrupted_row(move || {
                                                label().unwrap_or_default().to_string()
                                            })
                                        },
                                    ))
                                    .child(when(
                                        move || error.with(Option::is_some),
                                        move || error_row(error, time),
                                    ))
                                    .child(when(message, move || {
                                        // As T3 Code's: the user's message in its
                                        // bubble, other text in the lane, inset.
                                        view()
                                            .max_w(relative(1.0))
                                            .style_when(user, |s| {
                                                s.max_w(relative(0.8))
                                                    .p(px(12.0))
                                                    .rounded(px(18.0))
                                                    .bg(look::BUBBLE)
                                            })
                                            .style_when(
                                                move || !user(),
                                                |s| s.w_full().px(px(4.0)).py(px(2.0)),
                                            )
                                            .child(crate::rich_text::message(
                                                surroundings,
                                                move || label().map(|text| text.to_string()),
                                                kind,
                                                move || {
                                                    if user() {
                                                        (column_width.try_get().unwrap_or_default()
                                                            * 0.8
                                                            - 24.0)
                                                            .max(0.0)
                                                    } else {
                                                        (column_width.try_get().unwrap_or_default()
                                                            - 8.0)
                                                            .max(0.0)
                                                    }
                                                },
                                            ))
                                    }))
                                    .child(each(
                                        move || {
                                            row.with(|row| {
                                                row.as_ref()
                                                    .map(|row| row.images.clone())
                                                    .unwrap_or_default()
                                            })
                                        },
                                        |source: &String| source.clone(),
                                        move |source, _| {
                                            crate::media::image_view(
                                                connection,
                                                surroundings.preview,
                                                move || source.get(),
                                                if untracked(user) {
                                                    crate::media::ImageSize::Box(320.0, 240.0)
                                                } else {
                                                    crate::media::ImageSize::Natural
                                                },
                                            )
                                        },
                                    ))
                                    .child(when(
                                        move || {
                                            row.with(|row| {
                                                row.as_ref().is_some_and(|row| {
                                                    row.actions && row.shape != Shape::Actions
                                                })
                                            })
                                        },
                                        move || response_actions(connection, row, false),
                                    ))
                                    .child(when(
                                        move || shape() == Some(Shape::Actions),
                                        move || response_actions(connection, row, true),
                                    ))
                                    .child(when(user, move || user_actions(connection, row)))
                                    .child(when(
                                        move || {
                                            row.with(|row| {
                                                row.as_ref()
                                                    .is_some_and(|row| row.unresolved.is_some())
                                            })
                                        },
                                        move || unresolved_actions(connection, row),
                                    )),
                            )
                            .child(when(
                                move || {
                                    row.with(|row| {
                                        row.as_ref().is_some_and(|row| row.retry.is_some())
                                    })
                                },
                                move || {
                                    crate::ui::row_text_button(tr("再試行"), move || {
                                        following.set(true);
                                        if let Some(retry) = row.with_untracked(|row| {
                                            row.as_ref().and_then(|row| row.retry.clone())
                                        }) {
                                            connection.dispatch(Intent::RetryPreparation(retry));
                                        }
                                    })
                                },
                            ))
                    },
                )
                // Open at the end in the first layout. Jumping after the first
                // commit shows older rows at placeholder heights on the way.
                .initial_scroll_at_end()
                .keys(move || {
                    rows.with(|rows| rows.iter().map(|r| r.id.clone()).collect::<Vec<_>>())
                })
                .element_ref(&list)
                .label(SharedString::from(tr("この会話")))
                .estimated_item_size(px(170.0))
                // While it follows, the list keeps its end as rows grow; a
                // reader who scrolled away keeps their place.
                .follow_end(move || px(if following.get() { f32::MAX } else { 0.0 }))
                .on_scroll(move |event: &ScrollEvent| {
                    use agent_core::presentation::conversation::{ScrollPosition, follows_end};
                    // As T3 Code's, the selection's actions go while it scrolls.
                    #[cfg(desktop)]
                    surroundings.selection.dismiss();
                    let now = ScrollPosition {
                        offset: event.offset.y.0,
                        viewport: event.size.height.0,
                        content: event.content_size.height.0,
                    };
                    let next =
                        follows_end(following.get_untracked(), scrolled.replace(Some(now)), now);
                    if next != following.get_untracked() {
                        following.set(next);
                    }
                    let away = agent_core::presentation::conversation::away_from_end(now);
                    if away != away_from_end.get_untracked() {
                        away_from_end.set(away);
                    }
                })
                .on_viewable_items_change(move |items: &ViewableItems| {
                    if items.viewable.is_some() {
                        viewable.set(items.viewable);
                    }
                })
                .flex_1()
                .min_h_0()
            },
        ))
        .child(when(
            move || !following.get() && away_from_end.get(),
            move || {
                // Match the iPhone's 44-point glass circle, centered six
                // points above the composer.
                let label = tr("最新のメッセージへ");
                nsh_base::button()
                    .accessibility_label(Some(SharedString::from(label)))
                    .absolute()
                    .self_center()
                    .bottom(move || px(6.0 + covered.get()))
                    .w(px(44.0))
                    .h(px(44.0))
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .glass_effect(nsh_core::GlassEffect::Regular)
                    .text_color(look::FOREGROUND)
                    .on_press(move || {
                        following.set(true);
                        list.scroll_to_end(true);
                    })
                    .child(crate::ui::icon(crate::ui::Icon::ARROW_DOWN, 20.0))
            },
        ))
}

/// T3 Code's timeline minimap: at the conversation's leading edge, a strip
/// for each user message on a rail at its middle, lit while the message is
/// in view. Strips widen toward the pointer, which previews the exchange; a
/// strip jumps to its message. It stays shown beside a wide enough gutter
/// (`minimap_persistent`), otherwise while the pointer is over it.
fn minimap(timeline: Timeline, canvas: impl Fn() -> f32 + Copy + 'static) -> View {
    use agent_core::presentation::conversation::{
        MINIMAP_STRIP_INSET, minimap_hit_width, minimap_marker_width, minimap_markers,
        minimap_persistent, minimap_rail_height,
    };
    let Timeline {
        rows,
        viewable,
        list,
        following,
        width,
        ..
    } = timeline;
    let hovered = signal(None::<usize>);
    let over = signal(false);
    let user_rows = computed(move || {
        rows.with(|rows| {
            rows.iter()
                .enumerate()
                .filter(|(_, row)| row.kind == Kind::User)
                .map(|(index, _)| index)
                .collect::<Vec<_>>()
        })
    });
    // T3 Code's rail is at most the window's height less 18rem.
    let max_height = move || window().height.0 - 288.0;
    let markers = computed(move || {
        user_rows.with(|rows| minimap_markers(rows, viewable.get(), max_height()))
    });
    // Its strips' pointer area stops at the lane, at least the strip itself.
    let hit = move || {
        minimap_hit_width((width.get() - timeline.lane()) / 2.0).max(minimap_marker_width(None))
    };
    let exchange = move |row: usize| {
        rows.with(|rows| {
            let user = rows
                .get(row)
                .map(|r| r.body.to_string())
                .unwrap_or_default();
            let answer = rows
                .iter()
                .skip(row + 1)
                .take_while(|r| r.kind != Kind::User)
                .filter(|r| r.kind == Kind::Response)
                .last()
                .map(|r| r.body.to_string());
            (user, answer)
        })
    };
    view()
        .absolute()
        .left(px(0.0))
        .top(px(0.0))
        .bottom(px(0.0))
        .w(px(72.0))
        .justify_center()
        // As T3 Code's: only the rail takes the pointer, never the timeline
        // or composer it lies over.
        .pointer_events(PointerEvents::BoxNone)
        .opacity(move || {
            if minimap_persistent(canvas(), timeline.lane()) || over.get() {
                1.0_f32
            } else {
                0.0_f32
            }
        })
        .child(when(
            move || markers.with(|markers| !markers.is_empty()),
            move || {
                view()
                    .on_mouse_enter(move |_: &MouseEvent| over.set(true))
                    .on_mouse_leave(move |_: &MouseEvent| over.set(false))
                    .ml(px(MINIMAP_STRIP_INSET))
                    .w(move || px(hit()))
                    .h(move || px(minimap_rail_height(markers.with(Vec::len), max_height())))
                    .child(each(
                        move || markers.get(),
                        |marker: &agent_core::presentation::conversation::MinimapMarker| marker.row,
                        move |marker, position| {
                            let distance =
                                move || hovered.get().map(|h| h.abs_diff(position.get()));
                            strip(marker, distance, hit, exchange, move || {
                                following.set(false);
                                let row = marker.with_untracked(|m| m.row) as u32;
                                list.scroll_to_index(
                                    row,
                                    nsh_core::geometry::Percentage(0.0),
                                    px(0.0),
                                    true,
                                );
                            })
                            .on_mouse_enter(move |_: &MouseEvent| {
                                hovered.set(Some(position.get_untracked()))
                            })
                            .on_mouse_leave(move |_: &MouseEvent| {
                                if hovered.get_untracked() == Some(position.get_untracked()) {
                                    hovered.set(None);
                                }
                            })
                        },
                    ))
            },
        ))
}

/// A minimap strip, centred on its place on the rail: T3 Code's 2 point line
/// in the muted color, the foreground while its message is in view, and the
/// exchange it jumps to previewed beside it while hovered.
fn strip(
    marker: ReadSignal<agent_core::presentation::conversation::MinimapMarker>,
    distance: impl Fn() -> Option<usize> + Copy + 'static,
    hit: impl Fn() -> f32 + Copy + 'static,
    exchange: impl Fn(usize) -> (String, Option<String>) + Copy + 'static,
    press: impl Fn() + 'static,
) -> View {
    use crate::ui::colors;
    use agent_core::presentation::conversation::minimap_marker_width;
    let hovered = move || distance() == Some(0);
    view()
        .absolute()
        .left(px(0.0))
        .top(move || px(marker.with(|m| m.top) - 4.0))
        .child(
            nsh_base::button()
                // Rows before it may load: its message is read as it is now.
                .accessibility_label(nsh_core::element::Prop::from(
                    (move || {
                        Some(SharedString::from(tr_format(
                            "この発言へ移動: {0}",
                            [&exchange(marker.with(|m| m.row)).0],
                        )))
                    })
                    .into_reactive(),
                ))
                .w(move || px(hit()))
                .h(px(8.0))
                .justify_center()
                .on_press(press)
                .child(
                    view()
                        .h(px(2.0))
                        .rounded_full()
                        .w(move || px(minimap_marker_width(distance())))
                        .bg(move || {
                            if marker.with(|m| m.in_view) {
                                // `bg-foreground/90`.
                                look::FOREGROUND.alpha(0.9)
                            } else if hovered() {
                                // `bg-muted-foreground/75`.
                                look::MUTED.alpha(0.75)
                            } else {
                                look::MUTED.alpha(0.35)
                            }
                        }),
                ),
        )
        .child(when(hovered, move || {
            let (user, answer) = exchange(marker.with_untracked(|m| m.row));
            view()
                .absolute()
                .left(px(20.0))
                .top(px(-24.0))
                .w(px(320.0))
                .p_3()
                .gap_1()
                .rounded(px(12.0))
                .bg(colors::RAISED)
                .border_1()
                .border_color(colors::BORDER)
                .child(
                    text(user)
                        .text_size(px(look::TEXT))
                        .font_weight(FontWeight::MEDIUM)
                        .number_of_lines(1),
                )
                .child(
                    text(answer.unwrap_or_default())
                        .text_size(px(look::TEXT))
                        .text_color(look::MUTED)
                        .number_of_lines(3),
                )
        }))
}

/// A completed turn's folded work, as T3 Code's: its label and disclosure,
/// its time at the end while the pointer is over it, ruled below.
fn fold_row(
    label: impl Fn() -> String + 'static,
    open: impl Fn() -> bool + Copy + 'static,
    time: impl Fn() -> String + 'static,
    toggle: impl Fn() + 'static,
) -> View {
    use crate::ui::{Icon, icon};
    view()
        .w_full()
        .group()
        .flex_row()
        .items_center()
        .gap_1()
        .pt_1()
        .pb_2()
        .pr(px(2.0))
        .border_b_1()
        .border_b_color(look::RULE)
        .child(
            nsh_base::button()
                .flex_row()
                .items_center()
                .gap_1()
                .px_1()
                .rounded(px(6.0))
                .text_color(look::MUTED)
                .hover(|s| s.text_color(look::FOREGROUND))
                .on_press(toggle)
                .child(
                    text(label)
                        .text_size(px(look::TEXT))
                        .line_height(px(look::LINE)),
                )
                .child(when(open, || icon(Icon::CHEVRON_DOWN, 14.0)))
                .child(when(move || !open(), || icon(Icon::CHEVRON_RIGHT, 14.0))),
        )
        .child(view().flex_1())
        .child(hover_time(time))
}

/// A running turn's header, as T3 Code's: how long it has worked, ruled below.
fn working_row(label: impl Fn() -> String + 'static) -> View {
    view()
        .w_full()
        .pt_1()
        .pb_2()
        .border_b_1()
        .border_b_color(look::RULE)
        .child(
            view().h(px(24.0)).px_1().flex_row().items_center().child(
                text(label)
                    .text_size(px(look::TEXT))
                    .line_height(px(look::LINE))
                    .text_color(look::MUTED),
            ),
        )
}

/// Work under its summary, as T3 Code's work toggle: the work's icon, the
/// summary, its time while the pointer is over it.
fn group_row(
    row: Memo<Option<Row>>,
    time: impl Fn() -> String + 'static,
    toggle: impl Fn() + 'static,
) -> nsh_base::ButtonElement {
    let summary = move || {
        row.with(|row| row.as_ref().map(|row| row.body.to_string()))
            .unwrap_or_default()
    };
    // A reused cell shows another group's icon.
    let kind = move || {
        vec![row.with(|row| {
            row.as_ref()
                .map(|row| row.group_kind.clone())
                .unwrap_or_default()
        })]
    };
    nsh_base::button()
        .accessibility_label(nsh_core::element::Prop::from(
            (move || Some(SharedString::from(summary()))).into_reactive(),
        ))
        .group()
        .w_full()
        .flex_row()
        .items_center()
        .min_h(px(24.0))
        // T3 Code's work log row, open or not: `py-0.5`.
        .py(px(2.0))
        .px(px(2.0))
        .rounded(px(6.0))
        .on_press(toggle)
        .child(crate::activity::line(
            view().text_color(look::ICON).child(each(
                kind,
                |kind: &String| kind.clone(),
                |kind, _| {
                    crate::ui::icon(
                        crate::ui::Icon::work(&kind.get_untracked()),
                        look::ICON_SIZE,
                    )
                },
            )),
            text(summary)
                .flex_1()
                .min_w_0()
                .number_of_lines(1)
                .text_size(px(look::TEXT))
                .line_height(px(look::LINE))
                .text_color(look::MUTED),
        ))
        .child(hover_time(time))
}

/// A row's time, shown while the pointer is over the row (T3 Code's
/// `TimelineRowTimestamp`).
/// A row's controls: shown while the pointer is over the row, as T3 Code's,
/// or always (`shown`); a touch screen, with no pointer to rest, always shows
/// them (`pointer-coarse:opacity-100`).
fn revealed(view: View, shown: bool) -> View {
    view.opacity(if shown || !is_desktop() { 1.0 } else { 0.0 })
        .group_hover(|s| s.opacity(1.0))
}

fn hover_time(time: impl Fn() -> String + 'static) -> View {
    revealed(view(), false).child(text(time).text_size(px(12.0)).text_color(look::MUTED))
}

/// T3 Code's divider for an interrupted run: its mark and label in the
/// destructive color between rules.
/// A proposed plan, as T3 Code's plan card: the Plan badge and its title over
/// its Markdown, its actions behind the menu at the end.
fn plan_card(
    surroundings: crate::rich_text::Surroundings,
    row: Memo<Option<Row>>,
    column_width: Memo<f32>,
    saver: crate::plans::Saver,
) -> View {
    use crate::ui::colors;
    let read = move |read: fn(&Row) -> SharedString| {
        move || {
            row.try_get()
                .flatten()
                .as_ref()
                .map(read)
                .unwrap_or_default()
        }
    };
    let note = read(|row| row.note.clone());
    view().w_full().px(px(4.0)).py(px(2.0)).child(
        view()
            .w_full()
            .rounded(px(24.0))
            .border_1()
            .border_color(look::CARD_BORDER)
            .bg(colors::SURFACE)
            .p(px(20.0))
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .child(
                                // T3 Code's `secondary` badge.
                                view()
                                    .h(px(18.0))
                                    .px(px(5.0))
                                    .justify_center()
                                    .rounded(px(4.0))
                                    .border_1()
                                    .border_color(look::CARD_BORDER)
                                    .bg(look::CONTROL)
                                    .child(
                                        text(tr("計画"))
                                            .text_size(px(12.0))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(look::FOREGROUND),
                                    ),
                            )
                            .child(
                                text(read(|row| row.body.clone()))
                                    .text_size(px(look::TEXT))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(look::FOREGROUND),
                            ),
                    )
                    .child(crate::plans::actions(
                        saver,
                        surroundings.connection,
                        move || row.try_get().flatten().map(|row| row.note.to_string()),
                    )),
            )
            .child(view().mt(px(16.0)).child(crate::rich_text::message(
                surroundings,
                move || Some(note().to_string()),
                || Kind::Response,
                move || (column_width.try_get().unwrap_or_default() - 8.0 - 42.0).max(0.0),
            ))),
    )
}

/// A subagent the turn started, as T3 Code's subagent row: its avatar, its
/// name over its latest word.
fn subagent_row(
    connection: Connection,
    row: Memo<Option<Row>>,
    clock: Signal<std::time::SystemTime>,
) -> impl IntoElement {
    use crate::ui::{Icon, colors};
    let agent = move || row.try_get().flatten().and_then(|row| row.agent);
    let target = move || agent().and_then(|agent| agent.target);
    // Opening the child's history in the cache subscribes to its updates without
    // navigating away from the parent or touching its draft.
    let requested = stored(None);
    effect(move || {
        let target = target();
        if requested.replace(target.clone()) != target
            && let Some(target) = target
        {
            connection.dispatch(Intent::ReadThread(op::ReadThread::new(target)));
        }
    });
    // Read the cell's retained row directly. Chained memos created inside this
    // conditional branch can be queued while folding disposes their sources.
    let child = move || {
        let target = target()?;
        connection
            .publication
            .try_get()?
            .snapshot
            .sessions
            .conversations
            .get(&target)
            .cloned()
            .map(Shared)
    };
    let name = move || {
        agent()
            .map(|agent| agent.name(child().as_deref()))
            .unwrap_or_default()
    };
    let elapsed = move || {
        agent_core::presentation::agents::subagent_elapsed(
            crate::l10n::language(),
            child().as_deref(),
            clock
                .try_get()
                .unwrap_or_else(std::time::SystemTime::now)
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
        )
        .unwrap_or_default()
    };
    let status = move || agent().map(|agent| agent.status(child().as_deref()));
    nsh_base::button()
        .accessibility_label(nsh_core::element::Prop::from(
            (move || Some(SharedString::from(name()))).into_reactive(),
        ))
        .disabled(move || target().is_none())
        .on_press(move || {
            if let Some(target) = target() {
                connection.dispatch(Intent::ReadThread(op::ReadThread::open(target)));
            }
        })
        .w_full()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .px(px(8.0))
        .py(px(6.0))
        .child(
            view()
                .w(px(24.0))
                .h(px(24.0))
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .border_color(look::CARD_BORDER)
                .bg(look::AVATAR)
                .text_color(look::MUTED)
                .child(crate::ui::icon(Icon::BOT, 14.0))
                .child(
                    view()
                        .absolute()
                        .right(px(-1.0))
                        .bottom(px(-1.0))
                        .w(px(8.0))
                        .h(px(8.0))
                        .rounded_full()
                        .border_2()
                        .border_color(colors::BACKGROUND)
                        .bg(move || match status() {
                            Some(agent_protocol::models::TurnStatus::Completed) => look::SUCCESS,
                            Some(agent_protocol::models::TurnStatus::Running) => colors::ACCENT,
                            Some(agent_protocol::models::TurnStatus::Failed) => look::DESTRUCTIVE,
                            _ => look::MUTED,
                        }),
                ),
        )
        .child(
            view()
                .flex_1()
                .min_w_0()
                .child(
                    text(name)
                        .number_of_lines(1)
                        .text_size(px(12.0))
                        .line_height(px(16.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(look::FOREGROUND),
                )
                .child(
                    text(move || agent().map(|agent| agent.message).unwrap_or_default())
                        .text_size(px(11.0))
                        .line_height(px(18.0))
                        .text_color(look::MUTED),
                ),
        )
        .child(
            text(elapsed)
                .text_size(px(11.0))
                .text_color(look::MUTED)
                .flex_shrink_0(),
        )
}

/// A divider across the work, as T3 Code's context compaction.
fn divider_row(label: impl Fn() -> String + 'static) -> View {
    let rule = || view().flex_1().h(px(1.0)).bg(look::RULE);
    view()
        .w_full()
        .flex_row()
        .items_center()
        .gap_3()
        .py_1()
        .text_color(look::MUTED)
        .child(rule())
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .child(crate::ui::icon(crate::ui::Icon::MINIMIZE, 12.0))
                .child(text(label).text_size(px(12.0)).line_height(px(16.0))),
        )
        .child(rule())
}

fn interrupted_row(label: impl Fn() -> String + 'static) -> View {
    let rule = || view().flex_1().h(px(1.0)).bg(look::RULE);
    view()
        .w_full()
        .flex_row()
        .items_center()
        .gap_2()
        .py_2()
        .child(rule())
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .px_2()
                .py_1()
                .text_color(look::DESTRUCTIVE)
                .child(crate::ui::icon(crate::ui::Icon::CLOSE, 12.0))
                .child(
                    text(label)
                        .text_size(px(11.0))
                        .font_weight(FontWeight::MEDIUM),
                )
                // T3 Code's detail, the Host's reason; Bex's turns stop at
                // the user's word.
                .child(
                    text(format!("·\u{a0}{}", tr("ユーザーが実行を中断しました")))
                        .text_size(px(11.0))
                        .opacity(0.7),
                ),
        )
        .child(rule())
}

/// An opened folded turn whose work did not load says why where that work
/// would show, as T3 Code's "Couldn't load output" in an opened row, and
/// loads it again on retry.
fn work_load_failure(
    connection: Connection,
    row: Memo<Option<Row>>,
    open: impl Fn() -> bool + Copy + 'static,
) -> View {
    let load = move || {
        row.with(|row| {
            row.as_ref()
                .and_then(|row| row.activity.as_ref())
                .and_then(|activity| activity.load_items.clone())
        })
    };
    let failure = move || {
        load()
            .and_then(|load| {
                connection
                    .context
                    .with(|context| context.turn_items_error(load.turn_id))
            })
            .map(|error| error.to_string())
    };
    view().child(when(
        move || open() && failure().is_some(),
        move || {
            view()
                .pt_2()
                .gap_2()
                .items_start()
                .child(
                    text(move || failure().unwrap_or_default())
                        .selectable(true)
                        .text_size(px(13.0))
                        .text_color(nsh_core::element::rgb(0xff8e86)),
                )
                .child(crate::ui::text_button(tr("再試行"), move || {
                    if let Some(load) = untracked(load) {
                        connection.dispatch(Intent::LoadTurnItems(load));
                    }
                }))
        },
    ))
}

/// A selected conversation whose history has not arrived: it is loading, or
/// its read failed and can be retried.
fn opening(connection: Connection) -> View {
    use crate::ui::{self, colors};
    use agent_core::presentation::context::ConversationOpening;
    let opening = computed(move || connection.context.with(|p| p.opening()));
    let failure = move || match opening.get() {
        Some(ConversationOpening::Failed { error }) => Some(error),
        _ => None,
    };
    view()
        .w_full()
        .items_center()
        .child(when(
            move || opening.get() == Some(ConversationOpening::Loading),
            || {
                view()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .py_8()
                    .text_color(colors::MUTED)
                    .child(ui::spinner())
                    .child(text(tr("会話を読み込み中…")).text_size(px(14.0)))
            },
        ))
        .child(when(
            move || failure().is_some(),
            move || {
                view()
                    .items_center()
                    .gap_2()
                    .py_8()
                    .px_6()
                    .child(
                        text(move || failure().map(|error| error.to_string()).unwrap_or_default())
                            .selectable(true),
                    )
                    .child(ui::text_button(tr("再試行"), move || {
                        if let Some(thread) = connection
                            .context
                            .with_untracked(|p| p.navigation().thread_id)
                        {
                            connection.dispatch(Intent::ReadThread(op::ReadThread::open(thread)));
                        }
                    }))
            },
        ))
}

/// A turn's error, as T3 Code's provider error row: its alert and what
/// failed, its time at the end, the provider's message below; Bex's details
/// under it, and copying them all while the pointer is over the row.
fn error_row(
    error: Memo<Option<TurnErrorPresentation>>,
    time: impl Fn() -> String + 'static,
) -> View {
    use crate::ui::{self, Icon};
    use nsh_core::element::rgb;
    let read = move |read: fn(&TurnErrorPresentation) -> String| {
        move || error.with(|error| error.as_ref().map(read).unwrap_or_default())
    };
    let tone = move || {
        if error.with(|error| error.as_ref().is_some_and(|e| e.is_usage_limit)) {
            crate::ui::Tone::same(rgb(0xe5b567))
        } else {
            look::DESTRUCTIVE
        }
    };
    let details = read(|error| error.details.clone().unwrap_or_default());
    view()
        .w_full()
        .group()
        .px(px(2.0))
        .py(px(2.0))
        .child(
            view()
                .flex_row()
                .items_center()
                .min_h(px(24.0))
                .child(crate::activity::line(
                    view()
                        .text_color(tone)
                        .child(ui::icon(Icon::ALERT, look::ICON_SIZE)),
                    text(read(|error| error.title.clone()))
                        .flex_1()
                        .min_w_0()
                        .text_size(px(look::TEXT))
                        .line_height(px(look::LINE))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(tone),
                ))
                .child(revealed(view(), false).child(ui::icon_button_sized(
                    Icon::COPY,
                    tr("エラーの詳細をコピー"),
                    24.0,
                    12.0,
                    move || {
                        if let Some(error) = error.get_untracked() {
                            set_clipboard(format!(
                                "{}\n{}\n{}",
                                error.title,
                                error.message,
                                error.details.unwrap_or_default()
                            ));
                        }
                    },
                )))
                .child(
                    text(time)
                        .ml_2()
                        .text_size(px(12.0))
                        .text_color(look::MUTED),
                ),
        )
        .child(
            text(read(|error| error.message.clone()))
                .selectable(true)
                .ml(px(28.0))
                .py_1()
                .text_size(px(look::TEXT))
                .line_height(px(look::LINE))
                .text_color(look::MESSAGE),
        )
        .child(when(
            move || !details().is_empty(),
            move || {
                text(details)
                    .selectable(true)
                    .ml(px(28.0))
                    .number_of_lines(3)
                    .text_size(px(12.0))
                    .text_color(look::MUTED)
            },
        ))
}

/// Marks a message that waited in the queue before its turn (T3 Code's
/// "Queued", which has no icon).
fn queued_marker() -> View {
    use crate::ui::colors;
    view()
        .mr_1()
        .mb_1()
        .text_color(colors::MUTED)
        .label(tr("実行中の作業の後に送信しました"))
        .child(text(tr("キューから送信")).text_size(px(12.0)))
}

/// Marks a message that steered the working turn (T3 Code's "Steer").
fn steer_marker() -> View {
    use crate::ui::{self, Icon, colors};
    view()
        .flex_row()
        .items_center()
        .gap_1()
        .mr_1()
        .mb_1()
        .text_color(colors::MUTED)
        .label(tr("実行中の作業に追加で指示しました"))
        .child(ui::icon(Icon::STEER, 12.0))
        .child(text(tr("追加の指示")).text_size(px(12.0)))
}

/// T3 Code's message action: a 24 point ghost button, its 12 point icon.
fn action(
    icon: crate::ui::Icon,
    label: &'static str,
    press: impl Fn() + 'static,
) -> nsh_base::ButtonElement {
    // `ghost-muted`: the muted color, not the foreground.
    crate::ui::icon_button_sized(icon, tr(label), 24.0, 12.0, press).text_color(look::MUTED)
}

/// An answer's actions, as T3 Code's meta row under it while the pointer is
/// over the answer: fork from here, copy, its time. `trailing` the work that
/// ends its turn, they stay in view, closer to that work.
fn response_actions(connection: Connection, row: Memo<Option<Row>>, trailing: bool) -> View {
    use crate::ui::{Enabled, Icon};
    let fork = move || row.with(|row| row.as_ref().and_then(|row| row.fork.clone()));
    let thread = move || {
        connection
            .context
            .with(|context| context.navigation().thread_id)
    };
    let forking = move || {
        fork().and_then(|turn| connection.context.with(|context| context.fork_phase(turn)))
            == Some(op::OperationPhase::Running)
    };
    // As the desktop client's, a failed fork says why beside its button: Core
    // leaves it there rather than among the conversation's notices.
    let failure = move || {
        fork()
            .and_then(|turn| connection.context.with(|context| context.fork_error(turn)))
            .map(|error| error.to_string())
    };
    let actions = revealed(view().flex_row().items_center().gap_2(), trailing)
        .child(when(
            move || fork().is_some(),
            move || {
                action(Icon::GIT_FORK, "ここから会話を分岐", move || {
                    if let (Some(thread_id), Some(last_turn_id)) =
                        (untracked(thread), untracked(fork))
                    {
                        connection.dispatch(Intent::ForkSession(op::ForkSession {
                            thread_id,
                            last_turn_id,
                        }));
                    }
                })
                .enabled(move || !forking())
            },
        ))
        .child(action(Icon::COPY, "回答をコピー", move || {
            if let Some(row) = row.get_untracked() {
                set_clipboard(row.body.to_string());
            }
        }))
        .child(when(forking, || {
            text(tr("分岐を作成中…"))
                .text_size(px(12.0))
                .text_color(look::MUTED)
        }))
        .child(time(row));
    view()
        .px(px(4.0))
        .mt(px(if trailing { 2.0 } else { 6.0 }))
        .gap_1()
        .child(actions)
        .child(when(
            move || failure().is_some(),
            move || {
                text(move || failure().unwrap_or_default())
                    .selectable(true)
                    .text_size(px(12.0))
                    .text_color(look::DESTRUCTIVE)
            },
        ))
}

/// The user's own message's actions, as T3 Code's under its bubble while the
/// pointer is over it: its time, editing from it, copy.
fn user_actions(connection: Connection, row: Memo<Option<Row>>) -> View {
    use crate::ui::{Enabled, Icon};
    let edit_turn = move || row.with(|row| row.as_ref().and_then(|row| row.edit_turn.clone()));
    revealed(
        view().mt_1().pr_1().flex_row().items_center().gap_2(),
        false,
    )
    .child(time(row))
    .child(
        view()
            .flex_row()
            .items_center()
            .gap(px(2.0))
            .child(when(
                move || edit_turn().is_some(),
                move || {
                    action(Icon::PENCIL, "ここから編集", move || {
                        if let Some(turn) = untracked(edit_turn) {
                            edit_from_here(connection, turn);
                        }
                    })
                    .enabled(move || {
                        connection
                            .composer
                            .with(|c| c.controls.running_turn.is_none())
                            && !connection.context.with(|context| context.rewinding())
                    })
                },
            ))
            .child(action(Icon::COPY, "発言をコピー", move || {
                if let Some(text) =
                    row.with_untracked(|row| row.as_ref().map(|row| row.body.to_string()))
                {
                    set_clipboard(text);
                }
            })),
    )
}

/// T3 Code's confirmation before a conversation is rewound. Bex keeps no
/// per-turn file snapshot, so only the conversation goes back.
fn edit_from_here(connection: Connection, turn: agent_protocol::ids::TurnId) {
    let Some(thread_id) = connection
        .context
        .with_untracked(|context| context.navigation().thread_id)
    else {
        return;
    };
    spawn(async move {
        let confirmed = nsh_base::alert_dialog()
            .title(tr("ここから編集しますか？"))
            .description(tr(
                "この発言の前まで会話を巻き戻します。発言と添付は入力欄に戻ります。ファイルの変更はそのまま残ります。",
            ))
            .cancel("cancel", tr("キャンセル"))
            .destructive("rewind", tr("巻き戻す"))
            .show()
            .await
            .is_ok_and(|answer| answer.action.as_deref() == Some("rewind"));
        if confirmed {
            connection.dispatch(Intent::RewindSession(op::RewindSession::new(
                thread_id, turn,
            )));
            connection.composer_ref.focus();
        }
    });
}

/// A message the Host may not have received: back to the composer, or away.
fn unresolved_actions(connection: Connection, row: Memo<Option<Row>>) -> View {
    use crate::ui::{self, Icon};
    let id = move || row.with_untracked(|row| row.as_ref().and_then(|row| row.unresolved.clone()));
    view()
        .flex_row()
        .items_center()
        .gap_1()
        .child(ui::icon_button(
            Icon::PENCIL,
            tr("入力欄へ戻す"),
            move || {
                if let Some(id) = id() {
                    connection.dispatch(Intent::RestoreUnknownSubmission {
                        client_user_message_id: id.into(),
                    });
                }
            },
        ))
        .child(ui::icon_button(Icon::TRASH, tr("破棄"), move || {
            if let Some(id) = id() {
                connection.dispatch(Intent::DiscardUnknownSubmission {
                    client_user_message_id: id.into(),
                });
            }
        }))
}

fn time(row: Memo<Option<Row>>) -> Text {
    text(move || {
        row.with(|row| {
            row.as_ref()
                .and_then(|row| row.time.clone())
                .unwrap_or_default()
        })
    })
    .text_size(px(12.0))
    .text_color(look::MUTED)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation(
        status: &str,
        items: serde_json::Value,
    ) -> std::sync::Arc<RenderedConversation> {
        let source: agent_protocol::models::Thread = serde_json::from_value(serde_json::json!({
            "id": {"provider":"codex", "id":"thread"},
            "turns": [{"id":"turn", "status":status, "durationMs":2500, "items":items}]
        }))
        .unwrap();
        let key = source.id.as_ref().unwrap().into();
        agent_core::presentation::conversation::project_conversation(
            agent_core::localization::UiLanguage::Japanese,
            &key,
            &Default::default(),
            std::sync::Arc::new(source),
            &None,
        )
    }

    fn command(id: &str) -> serde_json::Value {
        serde_json::json!({"id":id, "status":"completed", "clientInputId":null,
            "body":{"inline":{"body":{"commandExecution":{
                "command":id, "cwd":null, "output":"", "exitCode":0}}}}})
    }

    fn text(id: &str, phase: &str) -> serde_json::Value {
        serde_json::json!({"id":id, "status":"completed", "clientInputId":null,
            "body":{"inline":{"body":{"assistantText":{"text":id, "phase":phase}}}}})
    }

    /// The rows' shapes and the space below each.
    fn layout(rows: &[Row]) -> Vec<(Shape, f32)> {
        rows.iter().map(|row| (row.shape, row.gap)).collect()
    }

    #[test]
    fn an_open_group_holds_its_tools_tight_and_a_lone_tool_keeps_its_space() {
        let conversation = conversation(
            "completed",
            serde_json::json!([
                text("checking", "commentary"),
                command("cargo build"),
                command("cargo test"),
                text("next", "commentary"),
                command("pwd"),
                text("done", "final_answer"),
            ]),
        );
        let folded = project(&conversation, &HashMap::new(), &HashSet::new());
        let fold = folded.iter().find_map(|row| row.activity.clone()).unwrap();
        let opened = [(fold.id.clone(), fold.status.clone())].into();
        let rows = project(&conversation, &opened, &HashSet::new());
        let group = rows
            .iter()
            .find(|row| matches!(row.shape, Shape::Group { .. }))
            .unwrap()
            .id
            .clone();
        let rows = project(&conversation, &opened, &[group].into());
        assert_eq!(
            layout(&rows),
            [
                (Shape::Fold { open: true }, 6.0),
                (Shape::Commentary, 8.0),
                (Shape::Group { open: true }, 0.0),
                (Shape::Work { member: true }, 0.0),
                (Shape::Work { member: true }, 4.0),
                (Shape::Commentary, 8.0),
                (Shape::Work { member: false }, 8.0),
                (Shape::Message, 16.0),
            ]
        );
    }

    #[test]
    fn a_running_turn_reads_as_its_header_and_its_live_work_or_thinking() {
        let running = conversation("running", serde_json::json!([command("pwd")]));
        let rows = project(&running, &HashMap::new(), &HashSet::new());
        assert_eq!(layout(&rows), [(Shape::Working, 6.0), (Shape::Live, 8.0)]);
        assert!(rows[0].progress.is_some());
        assert!(rows[1].detail.as_ref().is_some_and(|item| item.live));
        let answered = conversation(
            "running",
            serde_json::json!([command("pwd"), text("answer", "unknown")]),
        );
        let rows = project(&answered, &HashMap::new(), &HashSet::new());
        // As T3 Code, a Thinking row below the answer says the turn still works.
        assert_eq!(
            rows.last().and_then(|row| row.status),
            Some(crate::activity::Status::Thinking)
        );
    }
}
