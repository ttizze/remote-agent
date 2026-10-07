//! The conversation timeline: `ThreadView.rows` in a virtual list, with the
//! banners over it and the history control above it.
mod banners;
mod cards;
mod markdown;
mod message;
mod work;

use super::{
    Desktop, Views,
    ui::{color, icon, text_2xs, tint},
};
use agent_core::{
    state::Intent,
    view::{
        thread::{ThreadView, TimelineDisclosure},
        time::{TimestampFormat, chat_timestamp_tooltip, day_aware_timestamp},
        timeline::{
            rows::{TimelineRow, TimelineRowKind, TimelineUpdate, timeline_update},
            work_row::WorkLogRow,
        },
        working_status::{ThreadContentKind, thread_sync_label},
    },
};
use agent_domain::Timestamp;
use chrono::{DateTime, Local, TimeZone};
use gpui_kit::{
    component::{
        ActiveTheme, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        shimmer::ShimmerText,
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

/// How long a copy button shows its check mark.
const COPIED_FOR: Duration = Duration::from_secs(2);

pub(crate) struct TimelineState {
    disclosure: TimelineDisclosure,
    /// Item 0 is the history header; row `i` is item `i + 1`.
    list: ListState,
    /// The thread the list shows; `None` until views of one arrive.
    shown: Option<String>,
    /// User messages showing their full text.
    expanded_messages: HashSet<String>,
    /// Long plans showing in full.
    expanded_plans: HashSet<String>,
    /// The worktree setup card shows its details.
    setup_details: bool,
    /// Items whose withheld detail was asked for.
    requested_details: HashSet<String>,
    /// The expansion key each changed-files card's folder overrides belong to, by run.
    changed_files_keys: HashMap<String, String>,
    /// The copy button that last copied, until its check mark fades.
    copied: Option<(String, Instant)>,
}

impl TimelineState {
    pub(crate) fn new(_: &mut Window, _: &mut Context<Desktop>) -> Self {
        let list = ListState::new(1, ListAlignment::Bottom, px(1000.));
        list.set_follow_mode(FollowMode::Tail);
        Self {
            disclosure: TimelineDisclosure::default(),
            list,
            shown: None,
            expanded_messages: HashSet::new(),
            expanded_plans: HashSet::new(),
            setup_details: false,
            requested_details: HashSet::new(),
            changed_files_keys: HashMap::new(),
            copied: None,
        }
    }

    /// What the user expanded, which the rows are derived with.
    pub(crate) fn disclosure(&self) -> TimelineDisclosure {
        self.disclosure.clone()
    }

    /// Forgets the rows of the previous connection.
    pub(crate) fn reset(&mut self) {
        self.shown = None;
        self.list.reset(1);
        self.disclosure = TimelineDisclosure::default();
        self.forget_thread();
    }

    fn forget_thread(&mut self) {
        self.expanded_messages.clear();
        self.expanded_plans.clear();
        self.setup_details = false;
        self.requested_details.clear();
    }

    /// Starts showing `thread` from its end.
    fn show(&mut self, thread: &ThreadView) {
        if self.shown.as_deref() != Some(&thread.thread_id) {
            self.forget_thread();
        }
        self.shown = Some(thread.thread_id.clone());
        self.list.reset(thread.rows.len() + 1);
        self.list.set_follow_mode(FollowMode::Tail);
    }

    fn copied(&self, key: &str) -> bool {
        self.copied
            .as_ref()
            .is_some_and(|(copied, at)| copied == key && at.elapsed() < COPIED_FOR)
    }
}

/// Brings a list of rows (after its header item) to the next rows.
fn apply_update(list: &ListState, update: &TimelineUpdate) {
    let splice = &update.splice;
    if !splice.is_empty() {
        list.splice(
            splice.start as usize + 1..splice.end as usize + 1,
            splice.count as usize,
        );
    }
    for &index in &update.changed {
        let item = index as usize + 1;
        list.remeasure_items(item..item + 1);
    }
}

/// Adds `id` when missing, removes it when present.
fn toggle(values: &mut Vec<String>, id: &str) {
    match values.iter().position(|value| value == id) {
        Some(index) => {
            values.remove(index);
        }
        None => values.push(id.to_owned()),
    }
}

fn local_time(timestamp: &Timestamp) -> Option<DateTime<Local>> {
    Local.timestamp_millis_opt(timestamp.millis()).single()
}

/// The space under each row kind.
fn row_padding(row: &TimelineRow) -> f32 {
    let work_block = |header: bool| {
        if row.continues_work_log || header {
            0.
        } else {
            8.
        }
    };
    match &row.kind {
        TimelineRowKind::Work { expanded_group, .. } => {
            if row.continues_work_log {
                0.
            } else if *expanded_group {
                4.
            } else {
                8.
            }
        }
        TimelineRowKind::LiveWork { expanded, .. } | TimelineRowKind::Thinking { expanded, .. } => {
            work_block(*expanded)
        }
        TimelineRowKind::WorkToggle(toggle) => work_block(toggle.expanded),
        TimelineRowKind::Subagents(_) => work_block(false),
        TimelineRowKind::Working => 6.,
        TimelineRowKind::Fold(fold) => match fold.kind {
            agent_core::view::timeline::rows::FoldKind::Turn => 6.,
            agent_core::view::timeline::rows::FoldKind::Attempt => 8.,
        },
        TimelineRowKind::AssistantMessage(message) if message.meta.is_none() => 8.,
        TimelineRowKind::WorktreeSetup { .. }
        | TimelineRowKind::Lifecycle(_)
        | TimelineRowKind::Handoff(_) => 8.,
        _ => 16.,
    }
}

impl Desktop {
    pub(crate) fn render_timeline(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let views = self.views.clone();
        let Some(thread) = views.thread.as_ref() else {
            return div().flex_1().into_any_element();
        };
        if self.timeline.shown.as_deref() != Some(&thread.thread_id)
            || self.timeline.list.item_count() != thread.rows.len() + 1
        {
            self.timeline.show(thread);
        }
        let history = &thread.history;
        let empty = thread.rows.is_empty() && !history.has_more && history.error.is_none();
        let body =
            if empty {
                let label = thread_sync_label(thread.sync_status, ThreadContentKind::Loading);
                h_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_sm()
                            .text_color(match label {
                                Some(_) => color("textMuted"),
                                None => tint("textMuted", 0.3),
                            })
                            .child(label.unwrap_or_else(|| {
                                "Send a message to start the conversation.".into()
                            })),
                    )
                    .into_any_element()
            } else {
                let owner = cx.entity().downgrade();
                list(self.timeline.list.clone(), move |index, _, cx| {
                    owner
                        .update(cx, |view, cx| view.render_list_item(index, cx))
                        .unwrap_or_else(|_| div().into_any_element())
                })
                .size_full()
                .pb_4()
                .into_any_element()
            };
        let list = &self.timeline.list;
        let show_pill =
            !empty && !list.is_following_tail() && list.is_scrolled_to_end() == Some(false);
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(body)
            .child(self.render_timeline_banners(thread, cx))
            .when(show_pill, |timeline| {
                timeline.child(
                    h_flex()
                        .absolute()
                        .bottom(px(4.))
                        .left_0()
                        .right_0()
                        .justify_center()
                        .py(px(6.))
                        .child(
                            Button::new("timeline-scroll-to-end")
                                .icon(icon("chevron-down"))
                                .label("Scroll to end")
                                .xsmall()
                                .rounded(px(999.))
                                .bg(tint("surface", 0.8))
                                .border_1()
                                .border_color(color("border"))
                                .accessibility_label("Scroll to end")
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.timeline.list.set_follow_mode(FollowMode::Tail);
                                    view.timeline.list.scroll_to_end();
                                    cx.notify();
                                })),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_list_item(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let views = self.views.clone();
        let Some(thread) = views.thread.as_ref() else {
            return div().into_any_element();
        };
        let content = match index.checked_sub(1) {
            None => self.render_history_control(thread, cx),
            Some(row) => match thread.rows.get(row) {
                Some(row) => div()
                    .pb(px(row_padding(row)))
                    .child(self.render_row(thread, row, cx))
                    .into_any_element(),
                None => return div().into_any_element(),
            },
        };
        h_flex()
            .w_full()
            .justify_center()
            .px(px(20.))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .max_w(px(super::ui::metrics().chat_max_width))
                    .child(content),
            )
            .into_any_element()
    }

    /// The top spacer, with "Load earlier turns" while older turns exist.
    fn render_history_control(
        &mut self,
        thread: &ThreadView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let history = &thread.history;
        v_flex()
            .pt_4()
            .when(history.has_more || history.error.is_some(), |header| {
                header.pb_2().gap(px(6.))
            })
            .when(history.has_more, |header| {
                let loading = history.loading;
                header.child(
                    div()
                        .id("timeline-load-earlier")
                        .w_full()
                        .py(px(6.))
                        .flex()
                        .justify_center()
                        .text_xs()
                        .text_color(tint("textMuted", 0.6))
                        .when(!loading, |button| {
                            button
                                .cursor_pointer()
                                .hover(|style| style.text_color(color("text")))
                                .on_click(cx.listener(|view, _, _, _| {
                                    view.perform(Intent::LoadEarlier);
                                }))
                        })
                        .child(if loading {
                            "Loading earlier turns…"
                        } else {
                            "Load earlier turns"
                        }),
                )
            })
            .when_some(history.error.clone(), |header, error| {
                header.child(
                    div()
                        .w_full()
                        .flex()
                        .justify_center()
                        .text_xs()
                        .text_color(color("textMuted"))
                        .child(error),
                )
            })
            .into_any_element()
    }

    fn render_row(
        &mut self,
        thread: &ThreadView,
        row: &TimelineRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match &row.kind {
            TimelineRowKind::UserMessage(message) => self.render_user_message(row, message, cx),
            TimelineRowKind::AssistantMessage(message) => {
                self.render_assistant_message(thread, row, message, cx)
            }
            TimelineRowKind::AssistantMeta { message, meta } => {
                self.render_assistant_meta(thread, row, message.as_str(), meta, true, cx)
            }
            TimelineRowKind::PendingMessage(message) => {
                self.render_pending_message(row, message, cx)
            }
            TimelineRowKind::Work {
                rows,
                expanded_group,
            } => self.render_work(row, rows, *expanded_group, cx),
            TimelineRowKind::LiveWork {
                label,
                row: work,
                group_id,
                active,
                ..
            } => self.render_live_work(row, label, work, group_id, *active, cx),
            TimelineRowKind::WorkToggle(toggle) => self.render_work_toggle(row, toggle, cx),
            TimelineRowKind::Thinking { group_id, .. } => {
                self.render_thinking(thread, row, group_id.as_deref(), cx)
            }
            TimelineRowKind::Working => self.render_working(thread, row, cx),
            TimelineRowKind::Fold(fold) => self.render_fold(row, fold, cx),
            TimelineRowKind::ContextCompaction { label, active } => {
                work::render_compaction(&row.id, label, *active)
            }
            TimelineRowKind::Lifecycle(lifecycle) => self.render_lifecycle(row, lifecycle, cx),
            TimelineRowKind::Subagents(card) => self.render_subagents(row, card, cx),
            TimelineRowKind::Handoff(divider) => self.render_handoff(divider),
            TimelineRowKind::ProposedPlan(plan) => self.render_plan(row, plan, cx),
            TimelineRowKind::WorktreeSetup { snapshot, embedded } => {
                self.render_setup_row(thread, snapshot, *embedded, cx)
            }
        }
    }

    /// Brings the list from `previous` rows to the current ones.
    pub(crate) fn timeline_views_changed(
        &mut self,
        previous: &Views,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        let views = self.views.clone();
        let Some(thread) = views.thread.as_ref() else {
            if self.timeline.shown.take().is_some() {
                self.timeline.list.reset(1);
            }
            return;
        };
        let shown = previous.thread.as_ref().filter(|previous| {
            previous.thread_id == thread.thread_id
                && self.timeline.shown.as_deref() == Some(&thread.thread_id)
                && self.timeline.list.item_count() == previous.rows.len() + 1
        });
        match shown {
            Some(previous) if previous.rows_revision == thread.rows_revision => {}
            Some(previous) => {
                apply_update(
                    &self.timeline.list,
                    &timeline_update(&previous.rows, &thread.rows),
                );
            }
            None => self.timeline.show(thread),
        }
        self.load_requested_details(thread);
    }

    /// Asks for the withheld detail of every opened entry that needs it, once.
    fn load_requested_details(&mut self, thread: &ThreadView) {
        let loading: HashSet<String> = thread
            .rows
            .iter()
            .flat_map(|row| match &row.kind {
                TimelineRowKind::Work { rows, .. } => rows.iter().collect::<Vec<_>>(),
                TimelineRowKind::LiveWork { row, .. } => vec![row],
                _ => vec![],
            })
            .filter_map(|row| match row {
                WorkLogRow::Activity(activity) if activity.load_detail => Some(activity.id.clone()),
                _ => None,
            })
            .collect();
        for item_id in loading.difference(&self.timeline.requested_details) {
            self.perform(Intent::LoadItemDetail {
                item_id: item_id.clone(),
            });
        }
        self.timeline.requested_details = loading;
    }

    /// Flips one entry of the disclosure and derives the rows again.
    fn toggle_disclosure(
        &mut self,
        list: impl FnOnce(&mut TimelineDisclosure) -> &mut Vec<String>,
        id: &str,
        cx: &mut Context<Self>,
    ) {
        toggle(list(&mut self.timeline.disclosure), id);
        self.refresh_views(cx);
    }

    fn timestamp_format(&self) -> TimestampFormat {
        self.snapshot.preferences.timestamp_format
    }

    /// A message time, revealed on hover of `group` unless `always`, with its
    /// full date as the tooltip.
    fn render_timestamp(
        &self,
        id: impl Into<ElementId>,
        timestamp: Option<&Timestamp>,
        group: Option<SharedString>,
    ) -> Option<AnyElement> {
        let date = local_time(timestamp?)?;
        let format = self.timestamp_format();
        let label = day_aware_timestamp(&date, &Local::now(), format);
        let tooltip = SharedString::from(chat_timestamp_tooltip(&date, format));
        Some(
            div()
                .id(id)
                .flex_none()
                .whitespace_nowrap()
                .text_xs()
                .text_color(color("textMuted"))
                .when_some(group, |time, group| {
                    time.opacity(0.)
                        .group_hover(group, |style| style.opacity(1.))
                })
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .child(label)
                .into_any_element(),
        )
    }

    /// A ghost copy button that shows a check mark for a moment after copying.
    fn render_copy_button(&self, key: String, text: String, cx: &mut Context<Self>) -> Button {
        let copied = self.timeline.copied(&key);
        Button::new(SharedString::from(format!("copy-{key}")))
            .icon(icon(if copied { "check" } else { "copy" }))
            .ghost()
            .xsmall()
            .tooltip(if copied {
                "Copied!"
            } else {
                "Copy to clipboard"
            })
            .accessibility_label("Copy to clipboard")
            .on_click(cx.listener(move |view, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                view.timeline.copied = Some((key.clone(), Instant::now()));
                cx.notify();
            }))
    }
}

/// A label with the live-activity shimmer when `active`.
fn shimmer_label(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
) -> AnyElement {
    let label = label.into();
    if active {
        ShimmerText::new(label)
            .id(id)
            .duration(Duration::from_millis(2200))
            .highlight_color(color("text"))
            .into_any_element()
    } else {
        div().child(label).into_any_element()
    }
}

/// A 12px chevron pointing right, or down when open.
fn chevron(open: bool) -> impl IntoElement {
    icon(if open {
        "chevron-down"
    } else {
        "chevron-right"
    })
    .size(px(12.))
    .text_color(color("iconMuted"))
}

/// The theme's monospace family.
fn mono(cx: &App) -> SharedString {
    cx.theme().mono_font_family.clone()
}

/// `text-3xs` (10/14).
fn text_3xs<E: Styled>(element: E) -> E {
    element.text_size(px(10.)).line_height(px(14.))
}

#[cfg(test)]
mod tests {
    use super::{apply_update, toggle};
    use agent_core::view::timeline::rows::{TimelineRow, TimelineRowKind, timeline_update};
    use gpui_kit::{ListAlignment, ListOffset, ListState, px};

    fn rows(ids: &[&str]) -> Vec<TimelineRow> {
        ids.iter()
            .map(|id| TimelineRow {
                id: (*id).into(),
                created_at: None,
                continues_work_log: false,
                kind: TimelineRowKind::Working,
            })
            .collect()
    }

    fn list(rows: usize, top: usize, offset: f32) -> ListState {
        let list = ListState::new(rows + 1, ListAlignment::Bottom, px(600.));
        list.scroll_to(ListOffset {
            item_ix: top,
            offset_in_item: px(offset),
        });
        list
    }

    #[test]
    fn prepending_history_keeps_the_visible_row_anchored() {
        let (old, new) = (rows(&["a", "b"]), rows(&["history", "older", "a", "b"]));
        let list = list(old.len(), 2, 17.);
        apply_update(&list, &timeline_update(&old, &new));
        assert_eq!(list.item_count(), new.len() + 1);
        assert_eq!(list.logical_scroll_top().item_ix, 4);
        assert_eq!(list.logical_scroll_top().offset_in_item, px(17.));
    }

    #[test]
    fn rows_added_below_leave_the_anchor_alone() {
        let (old, new) = (rows(&["a", "b"]), rows(&["a", "b", "c"]));
        let list = list(old.len(), 1, 5.);
        apply_update(&list, &timeline_update(&old, &new));
        assert_eq!(list.item_count(), 4);
        assert_eq!(list.logical_scroll_top().item_ix, 1);
        assert_eq!(list.logical_scroll_top().offset_in_item, px(5.));
    }

    #[test]
    fn toggling_adds_then_removes() {
        let mut values = vec!["a".to_owned()];
        toggle(&mut values, "b");
        assert_eq!(values, ["a", "b"]);
        toggle(&mut values, "a");
        assert_eq!(values, ["b"]);
    }
}
