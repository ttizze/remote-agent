use crate::l10n::{tr, tr_format};
use crate::{Connection, shared::Shared};
use agent_core::{
    presentation::conversation::{
        ActivityPresentation, ConversationRow, ConversationRowContent, ItemSource, LANE_WIDTH,
        RenderedConversation, RenderedTurn, TurnErrorPresentation, TurnRow, WorkProgress,
        lane_padding, turn_rows,
    },
    state::{Intent, operations as op},
};
use nsh_core::prelude::*;
use std::collections::{HashMap, HashSet};

#[cfg(test)]
mod copy_tests;
pub(crate) mod native;

/// T3 Code's timeline as its dark theme draws it.
pub(crate) mod look {
    use crate::ui::colors;
    use nsh_core::element::{Rgba, rgb};
    pub(crate) fn muted() -> Rgba {
        colors::text(0xaaaaaa)
    }
    pub(crate) fn foreground() -> Rgba {
        colors::text(0xe6e6e6)
    }
    pub(crate) fn rule() -> Rgba {
        colors::line(0x161616)
    }
    pub(crate) const CODE: Rgba = rgb(0x111111);
    pub(crate) const DESTRUCTIVE: Rgba = rgb(0xfb414a);
    pub(crate) const TEXT: f32 = 14.0;
}

pub(crate) fn reveal_row(list: ElementRef, index: u32) {
    after_answer(move || {
        list.command("reveal", serde_json::json!({"row":index}));
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
    Preparation,
    /// A message: the user's, an answer, or text a client shows as it is.
    Message,
    /// Commentary between the work, without an answer's actions.
    Commentary,
    /// A completed turn's "Worked for …".
    Fold {
        open: bool,
    },
    /// A running turn's "Working for …".
    Working,
    /// Work under its summary.
    Group {
        open: bool,
    },
    /// A tool's row; a `member` of an open group sits tight in it.
    Work {
        member: bool,
    },
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
    preparation: Option<agent_core::presentation::conversation::PreparationPresentation>,
    id: String,
    body: SharedString,
    streaming: bool,
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
    notification: Option<agent_core::presentation::background_work::NotificationIcon>,
    agent: Option<agent_core::presentation::agents::SubagentPresentation>,
    error: Option<TurnErrorPresentation>,
    /// The message's id while the Host may not have received it: it goes back
    /// to the composer or is discarded, as the desktop client's controls do.
    unresolved: Option<String>,
}

impl Row {
    fn new(id: String, kind: Kind, shape: Shape, body: impl Into<SharedString>) -> Self {
        Self {
            preparation: None,
            id,
            body: body.into(),
            streaming: false,
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
    let native_end = rows.len();
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
    agent_core::presentation::conversation::rows::position_workspace_setup(
        &mut rows,
        native_end,
        |row| row.kind == Kind::User,
        |row| row.preparation.is_some(),
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
    let streaming = match &row {
        TurnRow::Row(ConversationRow {
            content: ConversationRowContent::Response { item, .. },
            ..
        })
        | TurnRow::Commentary(item) => matches!(&item.source, ItemSource::Native(native)
            if native.status == agent_protocol::models::ItemStatus::Running),
        _ => false,
    };
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
    let mut shown = match row {
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
    };
    shown.streaming =
        streaming && turn.source.status == agent_protocol::models::TurnStatus::Running;
    Some(shown)
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
        ConversationRowContent::Preparation { preparation } => (
            Kind::Activity,
            if preparation.steps.is_empty() {
                Shape::Message
            } else {
                Shape::Preparation
            },
            preparation.text.clone(),
        ),
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
            shown.retry = preparation.retry.clone();
            if !preparation.steps.is_empty() {
                shown.preparation = Some(preparation);
            } else if preparation.shimmer {
                shown.status = Some(crate::activity::Status::SettingUp);
            }
        }
        ConversationRowContent::Notification { icon, .. } => {
            shown.notification = Some(icon);
        }
        ConversationRowContent::Error { error } => shown.error = Some(error),
        _ => {}
    }
    Some(shown)
}
/// Shared timeline state; the platform viewport owns row measurements.
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
    fn set_activity(
        self,
        activity: ActivityPresentation,
        opening: bool,
    ) -> Option<op::LoadTurnItems> {
        self.expanded.update(|all| {
            if opening {
                all.insert(activity.id.clone(), activity.status);
            } else {
                all.remove(&activity.id);
            }
        });
        opening.then_some(activity.load_items).flatten()
    }

    fn toggle_group(self, id: String) {
        self.groups.update(|all| {
            if !all.remove(&id) {
                all.insert(id);
            }
        });
    }

    /// The loaded messages' texts, each with its row's place in the list.
    pub(crate) fn message_texts(self) -> Vec<(usize, String, String)> {
        self.rows.with(|rows| {
            rows.iter()
                .enumerate()
                .filter(|(_, row)| {
                    matches!(row.kind, Kind::User | Kind::Response)
                        && matches!(row.shape, Shape::Message | Shape::Commentary)
                })
                .map(|(index, row)| (index, row.id.to_string(), row.body.to_string()))
                .collect()
        })
    }

    /// Brings a row into view.
    pub(crate) fn reveal(self, row: usize) {
        self.following.set(false);
        reveal_row(self.list, row as u32);
    }

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

    pub(crate) fn list(self) -> AnyElement {
        native::view(self).into_element()
    }

    /// T3 Code's minimap at the leading edge of the conversation, beside its
    /// timeline and composer, on a canvas `canvas` points wide.
    pub(crate) fn minimap(self, canvas: impl Fn() -> f32 + Copy + 'static) -> impl IntoElement {
        when(is_desktop, move || minimap(self, canvas))
    }
}

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
                                reveal_row(list, row);
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
                                look::foreground().alpha(0.9)
                            } else if hovered() {
                                // `bg-muted-foreground/75`.
                                look::muted().alpha(0.75)
                            } else {
                                look::muted().alpha(0.35)
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
                .border_color(colors::border())
                .child(
                    text(user)
                        .text_size(px(look::TEXT))
                        .font_weight(FontWeight::MEDIUM)
                        .number_of_lines(1),
                )
                .child(
                    text(answer.unwrap_or_default())
                        .text_size(px(look::TEXT))
                        .text_color(look::muted())
                        .number_of_lines(3),
                )
        }))
}

/// Rewinding belongs to the conversation owner, including its confirmation.
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
