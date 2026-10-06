//! The mobile thread list (Home and the iPad sidebar): one flat list of
//! pinned and active cards, unsent new threads, then the Working, Snoozed and
//! Settled shelves, with each row's status, labels, swipe actions and menu.
mod menu;

pub use menu::{
    RowMenuContext, ThreadMenuAction, ThreadMenuItem, ThreadMenuOption, TitleRename,
    resolve_thread_title_rename, snooze_menu_options, thread_row_menu,
    title_regeneration_menu_item,
};

use super::inbox::{
    InboxReturns, is_thread_working, sort_inbox_threads_by_return, sort_working_threads_by_send,
};
use super::snooze::{
    QUEUED_TURN_START_GRACE_MS, SnoozePreset, SnoozePresetId, can_snooze, effective_snoozed,
    has_queued_turn_start, resolve_snooze_presets, snooze_wake_label,
};
use super::thread_order::{
    MoveAvailability, OrderSection, PendingThreadOrder, apply_pending_thread_order,
    move_availability,
};
use super::thread_sort::{
    settled_thread_timestamp, sort_active_threads_by_order_key, sort_pinned_threads_by_order_key,
    sort_settled_threads,
};
use super::thread_summary::{RuntimeStatus, SettledOverride, ThreadSummary};
use super::time::{TimestampFormat, relative_time};
use crate::commands::outbox::Request;
use crate::state::Snapshot;
use agent_domain::{Command, Driver, Timestamp};
use agent_protocol::conversation::WorkspaceStrategy;
use chrono::{DateTime, Local, TimeZone};
use std::collections::{BTreeMap, BTreeSet};

/// Settled rows shown before "Show more"; recent history is the common lookup.
pub const SETTLED_INITIAL_COUNT: u32 = 10;
/// Settled rows each "Show more" adds.
pub const SETTLED_PAGE_COUNT: u32 = 25;

static NO_IDS: BTreeSet<String> = BTreeSet::new();

/// Colour distinguishes approval, input, active work and failures; ready is
/// the unlabeled resting state, and waiting is the agent parked on open
/// background work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ThreadListStatus {
    Approval,
    Input,
    Working,
    Waiting,
    Failed,
    Limited,
    Ready,
}
impl ThreadListStatus {
    pub fn label(self) -> Option<&'static str> {
        match self {
            Self::Approval => Some("Approval"),
            Self::Input => Some("Input"),
            Self::Working => Some("Working"),
            Self::Failed => Some("Failed"),
            Self::Limited => Some("Limited"),
            Self::Waiting | Self::Ready => None,
        }
    }
}

pub fn thread_list_status(thread: &ThreadSummary) -> ThreadListStatus {
    if thread.has_pending_approvals {
        return ThreadListStatus::Approval;
    }
    if thread.has_pending_user_input {
        return ThreadListStatus::Input;
    }
    let Some(runtime) = &thread.runtime else {
        return ThreadListStatus::Ready;
    };
    match runtime.status {
        status if status.is_active() => ThreadListStatus::Working,
        RuntimeStatus::Idle => ThreadListStatus::Waiting,
        RuntimeStatus::Failed if runtime.last_error_class.as_deref() == Some("usage_limit") => {
            ThreadListStatus::Limited
        }
        RuntimeStatus::Failed => ThreadListStatus::Failed,
        _ => ThreadListStatus::Ready,
    }
}

/// Cards are pinned, active and working rows; slim rows are the receded
/// snoozed and settled shelves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum RowVariant {
    Card,
    Slim,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SwipeAction {
    Settle,
    Unsettle,
    Snooze,
    /// Wake a snoozed thread now.
    Unsnooze,
}
impl SwipeAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Settle => "Settle",
            Self::Unsettle => "Un-settle",
            Self::Snooze => "Snooze",
            Self::Unsnooze => "Wake",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwipeActions {
    /// A full swipe commits this lifecycle action.
    pub primary: SwipeAction,
    pub secondary: Option<SwipeAction>,
}

/// Settle on cards, un-settle on settled rows, wake on snoozed rows; snooze
/// joins unless the thread is snoozed or cannot be snoozed now.
pub fn resolve_swipe_actions(variant: RowVariant, snoozable: bool, snoozed: bool) -> SwipeActions {
    if snoozed {
        return SwipeActions {
            primary: SwipeAction::Unsnooze,
            secondary: None,
        };
    }
    SwipeActions {
        primary: match variant {
            RowVariant::Slim => SwipeAction::Unsettle,
            RowVariant::Card => SwipeAction::Settle,
        },
        secondary: snoozable.then_some(SwipeAction::Snooze),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SnoozeMenuSelection {
    Selected {
        preset: SnoozePreset,
    },
    /// The displayed wake time has passed; the user picks again.
    Expired,
}

/// A preset picked from a menu opened earlier: the preset as of now when it
/// is still offered, otherwise the displayed one while its time is ahead.
pub fn resolve_snooze_menu_selection<Tz: TimeZone>(
    preset: SnoozePresetId,
    displayed: &[SnoozePreset],
    now: &DateTime<Tz>,
    format: TimestampFormat,
) -> SnoozeMenuSelection {
    if let Some(current) = resolve_snooze_presets(now, format)
        .into_iter()
        .find(|candidate| candidate.id == preset)
    {
        return SnoozeMenuSelection::Selected { preset: current };
    }
    displayed
        .iter()
        .find(|candidate| candidate.id == preset)
        .filter(|candidate| {
            Timestamp::parse(&candidate.snoozed_until)
                .is_ok_and(|until| until.millis() > now.timestamp_millis())
        })
        .map_or(SnoozeMenuSelection::Expired, |preset| {
            SnoozeMenuSelection::Selected {
                preset: preset.clone(),
            }
        })
}

/// When a queued-turn snooze guard lapses on its own, so a row can offer
/// Snooze without waiting for unrelated data. `None` while only new data can
/// unblock it.
pub fn snooze_gate_expiry_ms(thread: &ThreadSummary, now_ms: i64) -> Option<i64> {
    if thread.has_pending_approvals || thread.has_pending_user_input {
        return None;
    }
    if !has_queued_turn_start(thread, now_ms) {
        return None;
    }
    Some(thread.latest_user_message_at? + QUEUED_TURN_START_GRACE_MS)
}

/// Provider drivers for a row's trailing icon stack, back to front. Instances
/// the configuration lacks are skipped, and an unknown current provider
/// yields nothing so a row never draws a stale stack.
pub fn provider_drivers(
    thread: &ThreadSummary,
    providers: &BTreeMap<String, Driver>,
) -> Vec<Driver> {
    let stack = thread.provider_stack();
    if stack
        .last()
        .is_none_or(|current| !providers.contains_key(current))
    {
        return vec![];
    }
    stack
        .iter()
        .filter_map(|instance| providers.get(instance).copied())
        .collect()
}

/// The texts a pull request search matches besides the title.
pub fn pull_request_search_terms(thread: &ThreadSummary) -> Vec<String> {
    thread
        .linked_pull_request
        .as_ref()
        .map(|pr| {
            vec![
                format!("#{}", pr.number),
                format!("{}#{}", pr.repository, pr.number),
                pr.url.clone(),
            ]
        })
        .unwrap_or_default()
}

fn matches_search(thread: &ThreadSummary, query: &str, matched: &BTreeSet<String>) -> bool {
    thread.title.to_lowercase().contains(query)
        || pull_request_search_terms(thread)
            .iter()
            .any(|term| term.to_lowercase().contains(query))
        || matched.contains(&thread.id)
}

fn listed(thread: &ThreadSummary) -> bool {
    thread.archived_at.is_none() && !thread.subagent
}

fn settled(thread: &ThreadSummary, queued: &BTreeSet<String>) -> bool {
    thread.settled_override == Some(SettledOverride::Settled) && !queued.contains(&thread.id)
}

/// The whole arranged section Move up/down works on, regardless of search or
/// project scope. A thread with an unsent message stays active.
pub fn ordered_section<'a>(
    threads: &'a [ThreadSummary],
    section: OrderSection,
    pending: Option<&PendingThreadOrder>,
    now_ms: i64,
    queued: &BTreeSet<String>,
) -> Vec<&'a ThreadSummary> {
    let rows: Vec<&ThreadSummary> = threads
        .iter()
        .filter(|thread| {
            listed(thread)
                && !settled(thread, queued)
                && !effective_snoozed(thread, now_ms)
                && thread.pinned_at.is_some() == (section == OrderSection::Pinned)
        })
        .collect();
    let ordered = match section {
        OrderSection::Pinned => sort_pinned_threads_by_order_key(rows),
        OrderSection::Active => sort_active_threads_by_order_key(rows),
    };
    let pending = pending
        .filter(|pending| pending.section == section)
        .and_then(|pending| pending.reconcile(&ordered));
    apply_pending_thread_order(ordered, section, pending.as_ref())
}

/// What partitions the list. `settled_shelf_expanded` defaults to true.
#[derive(Debug, Clone)]
pub struct ThreadListInput<'a> {
    pub threads: &'a [ThreadSummary],
    /// Second-precise, so a snooze wakes on time.
    pub now_ms: i64,
    pub pending_order: Option<&'a PendingThreadOrder>,
    pub project: Option<&'a str>,
    pub search_query: &'a str,
    /// Threads whose messages match the search.
    pub matched: Option<&'a BTreeSet<String>>,
    /// Threads with a message waiting to be delivered.
    pub queued: Option<&'a BTreeSet<String>>,
    /// Stays visible on a collapsed shelf so a split view keeps its row.
    pub selected: Option<&'a str>,
    /// Settled rows to build; the rest are only counted.
    pub settled_limit: Option<usize>,
    /// Working section beta: unpinned working threads fold into a shelf and
    /// the inbox orders by when each thread came back to the user.
    pub working_shelf_enabled: bool,
    pub working_shelf_expanded: bool,
    /// Returns this device saw but the Host does not stamp; read only while
    /// the beta is on.
    pub inbox_returns: Option<&'a InboxReturns>,
    pub snoozed_shelf_expanded: bool,
    pub settled_shelf_expanded: bool,
}
impl Default for ThreadListInput<'_> {
    fn default() -> Self {
        Self {
            threads: &[],
            now_ms: 0,
            pending_order: None,
            project: None,
            search_query: "",
            matched: None,
            queued: None,
            selected: None,
            settled_limit: None,
            working_shelf_enabled: false,
            working_shelf_expanded: false,
            inbox_returns: None,
            snoozed_shelf_expanded: false,
            settled_shelf_expanded: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutItem<'a> {
    pub thread: &'a ThreadSummary,
    pub variant: RowVariant,
    /// A snoozed-shelf row: shows the wake countdown and offers Wake.
    pub snoozed: bool,
    /// A pinned-block row: draws the pin and offers Unpin.
    pub pinned: bool,
    pub is_last: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadListLayout<'a> {
    pub items: Vec<LayoutItem<'a>>,
    /// Settled threads beyond the limit, behind "Show more".
    pub hidden_settled_count: usize,
    pub working_count: usize,
    /// Where the shelf header sits in `items`; a collapsed shelf keeps it.
    pub working_shelf_header_index: Option<usize>,
    pub snoozed_count: usize,
    pub snoozed_shelf_header_index: Option<usize>,
    /// Every settled thread in scope, including rows collapse or paging hide.
    pub settled_count: usize,
    pub settled_shelf_header_index: Option<usize>,
    /// The soonest wake, when the list must partition again.
    pub next_snooze_wake_at: Option<i64>,
}

/// Pinned and active cards in their saved order, then the Working, Snoozed
/// and Settled shelves. Snooze outranks settlement and pinning until the
/// thread wakes; a settled thread leaves the pinned block.
pub fn build_thread_list_layout<'a>(input: &ThreadListInput<'a>) -> ThreadListLayout<'a> {
    let now = input.now_ms;
    let queued = input.queued.unwrap_or(&NO_IDS);
    let pending = input.pending_order.and_then(|pending| {
        pending.reconcile(&ordered_section(
            input.threads,
            pending.section,
            None,
            now,
            queued,
        ))
    });
    let query = input.search_query.trim().to_lowercase();
    let matched = input.matched.unwrap_or(&NO_IDS);
    let (mut pinned, mut active, mut working, mut settled_rows, mut snoozed) =
        (vec![], vec![], vec![], vec![], vec![]);
    let mut next_snooze_wake_at: Option<i64> = None;
    for thread in input.threads {
        if !listed(thread)
            || input
                .project
                .is_some_and(|project| thread.project != project)
        {
            continue;
        }
        if !query.is_empty() && !matches_search(thread, &query, matched) {
            continue;
        }
        if effective_snoozed(thread, now) {
            if let Some(until) = thread.snoozed_until
                && next_snooze_wake_at.is_none_or(|next| until < next)
            {
                next_snooze_wake_at = Some(until);
            }
            snoozed.push(thread);
        } else if settled(thread, queued) {
            settled_rows.push(thread);
        } else if thread.pinned_at.is_some() {
            pinned.push(thread);
        } else if input.working_shelf_enabled && is_thread_working(thread) {
            working.push(thread);
        } else {
            active.push(thread);
        }
    }

    // The beta inbox is time-ordered: the saved arrangement and any move in
    // flight wait until the beta is off again.
    let ordered_active = if input.working_shelf_enabled {
        let no_returns = InboxReturns::default();
        sort_inbox_threads_by_return(active, input.inbox_returns.unwrap_or(&no_returns))
    } else {
        apply_pending_thread_order(
            sort_active_threads_by_order_key(active),
            OrderSection::Active,
            pending.as_ref(),
        )
    };
    let ordered_working = sort_working_threads_by_send(working);
    snoozed.sort_by_key(|thread| thread.snoozed_until.unwrap_or(0));
    let is_selected = |thread: &&ThreadSummary| Some(thread.id.as_str()) == input.selected;
    let shown = |rows: &[&'a ThreadSummary], expanded: bool| -> Vec<&'a ThreadSummary> {
        rows.iter()
            .filter(|thread| expanded || is_selected(thread))
            .copied()
            .collect()
    };
    let visible_working = shown(&ordered_working, input.working_shelf_expanded);
    let visible_snoozed = shown(&snoozed, input.snoozed_shelf_expanded);
    let ordered_settled = sort_settled_threads(settled_rows);
    let limit = input.settled_limit.unwrap_or(usize::MAX);
    let mut paged: Vec<&ThreadSummary> = ordered_settled.iter().take(limit).copied().collect();
    if let Some(selected) = ordered_settled[paged.len()..]
        .iter()
        .find(|thread| is_selected(thread))
    {
        paged.push(selected);
    }
    let visible_settled = shown(&paged, input.settled_shelf_expanded);

    let mut items: Vec<LayoutItem> = apply_pending_thread_order(
        sort_pinned_threads_by_order_key(pinned),
        OrderSection::Pinned,
        pending.as_ref(),
    )
    .into_iter()
    .map(|thread| LayoutItem {
        pinned: true,
        ..item(thread, RowVariant::Card, false)
    })
    .collect();
    items.extend(
        ordered_active
            .into_iter()
            .map(|thread| item(thread, RowVariant::Card, false)),
    );
    let header = |rows: &[&ThreadSummary], index: usize| (!rows.is_empty()).then_some(index);
    let working_shelf_header_index = header(&ordered_working, items.len());
    items.extend(
        visible_working
            .into_iter()
            .map(|thread| item(thread, RowVariant::Card, false)),
    );
    let snoozed_shelf_header_index = header(&snoozed, items.len());
    items.extend(
        visible_snoozed
            .into_iter()
            .map(|thread| item(thread, RowVariant::Slim, true)),
    );
    let settled_shelf_header_index = header(&ordered_settled, items.len());
    items.extend(
        visible_settled
            .into_iter()
            .map(|thread| item(thread, RowVariant::Slim, false)),
    );
    if let Some(last) = items.last_mut() {
        last.is_last = true;
    }
    ThreadListLayout {
        items,
        hidden_settled_count: ordered_settled.len() - paged.len(),
        working_count: ordered_working.len(),
        working_shelf_header_index,
        snoozed_count: snoozed.len(),
        snoozed_shelf_header_index,
        settled_count: ordered_settled.len(),
        settled_shelf_header_index,
        next_snooze_wake_at,
    }
}

fn item(thread: &ThreadSummary, variant: RowVariant, snoozed: bool) -> LayoutItem<'_> {
    LayoutItem {
        thread,
        variant,
        snoozed,
        pinned: false,
        is_last: false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SwipeButton {
    pub action: SwipeAction,
    pub label: String,
}
impl From<SwipeAction> for SwipeButton {
    fn from(action: SwipeAction) -> Self {
        Self {
            action,
            label: action.label().into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadRow {
    pub key: String,
    pub id: String,
    pub title: String,
    pub project_id: String,
    pub project_title: Option<String>,
    pub branch: Option<String>,
    pub variant: RowVariant,
    pub snoozed: bool,
    pub pinned: bool,
    pub selected: bool,
    pub is_last: bool,
    pub status: ThreadListStatus,
    /// The status word, or "Done" for a completion the user has not opened.
    pub status_label: Option<String>,
    pub unread: bool,
    /// Blank while a status label or the wake countdown takes its place.
    pub time_label: String,
    pub snooze_wake_label: Option<String>,
    /// When Snooze becomes available without new data; rebuild then.
    pub snooze_gate_expires_at_ms: Option<i64>,
    /// A message for this thread waits in the outbox.
    pub has_queued_messages: bool,
    pub can_move_up: bool,
    pub can_move_down: bool,
    /// An inset hairline under the row; section headers draw their own rule.
    pub show_trailing_divider: bool,
    /// Provider instances for the trailing icon stack, back to front.
    pub provider_instances: Vec<String>,
    pub search_snippet: Option<String>,
    pub swipe_primary: SwipeButton,
    pub swipe_secondary: Option<SwipeButton>,
    /// The "Snooze until" choices of the swipe and the menu; empty when the
    /// thread cannot be snoozed now.
    pub snooze_options: Vec<ThreadMenuOption>,
    /// The long-press menu.
    pub menu: Vec<ThreadMenuItem>,
}

/// A new thread the device sent but the Host has not listed yet.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PendingTaskRow {
    pub key: String,
    pub command_id: String,
    pub thread_id: String,
    pub project_id: String,
    pub title: String,
    pub branch: Option<String>,
    pub created_at_ms: i64,
    /// The first unsent row draws the section rule.
    pub show_pending_divider: bool,
    pub show_trailing_divider: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ShelfHeader {
    pub count: u32,
    pub expanded: bool,
    /// Shelf preferences are still loading.
    pub disabled: bool,
}

/// Rows cross the FFI boundary by value, so the thread row stays unboxed.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ThreadListItem {
    Thread { row: ThreadRow },
    PendingTask { task: PendingTaskRow },
    WorkingShelf { shelf: ShelfHeader },
    SnoozedShelf { shelf: ShelfHeader },
    SettledShelf { shelf: ShelfHeader },
}
impl ThreadListItem {
    /// Stable across rebuilds, for list diffing.
    pub fn key(&self) -> &str {
        match self {
            Self::Thread { row } => &row.key,
            Self::PendingTask { task } => &task.key,
            Self::WorkingShelf { .. } => "working-shelf",
            Self::SnoozedShelf { .. } => "snoozed-shelf",
            Self::SettledShelf { .. } => "settled-shelf",
        }
    }
}

pub struct ListItemsInput<'a> {
    pub layout: &'a ThreadListLayout<'a>,
    pub pending_tasks: Vec<PendingTaskRow>,
    pub now_ms: i64,
    /// The minute clock wake countdowns read.
    pub wake_label_now_ms: i64,
    pub snooze_presets: &'a [SnoozePreset],
    pub queued: Option<&'a BTreeSet<String>>,
    /// Consulted for cards only; slim menus omit the moves.
    pub move_availability: Option<&'a BTreeMap<String, MoveAvailability>>,
    pub selected: Option<&'a str>,
    /// Active cards follow a saved arrangement (the Working beta orders them
    /// by time instead).
    pub active_reorderable: bool,
    pub working_shelf_expanded: bool,
    pub snoozed_shelf_expanded: bool,
    pub settled_shelf_expanded: bool,
    pub shelf_preferences_loading: bool,
}

fn thread_row(item: &LayoutItem, input: &ListItemsInput, queued: &BTreeSet<String>) -> ThreadRow {
    let thread = item.thread;
    let snooze_wake_label = item
        .snoozed
        .then_some(thread.snoozed_until)
        .flatten()
        .map(|until| snooze_wake_label(until, input.wake_label_now_ms));
    let status = thread_list_status(thread);
    let unseen = thread.has_unseen_completion();
    let unread = status == ThreadListStatus::Ready && unseen;
    let time_label = if snooze_wake_label.is_some()
        || (item.variant == RowVariant::Card && (status != ThreadListStatus::Ready || unseen))
    {
        String::new()
    } else {
        let at = if item.variant == RowVariant::Slim && !item.snoozed {
            settled_thread_timestamp(thread)
        } else {
            thread.latest_user_message_at.unwrap_or(thread.updated_at)
        };
        relative_time(at, input.now_ms)
    };
    let snoozable = !item.snoozed && can_snooze(thread, input.now_ms);
    let swipe = resolve_swipe_actions(item.variant, snoozable, item.snoozed);
    let moves = (item.variant == RowVariant::Card)
        .then(|| input.move_availability?.get(&thread.id).copied())
        .flatten()
        .unwrap_or_default();
    let snooze_options = if snoozable {
        snooze_menu_options(input.snooze_presets)
    } else {
        vec![]
    };
    let menu = thread_row_menu(
        thread,
        &RowMenuContext {
            variant: item.variant,
            snoozed: item.snoozed,
            reorderable: item.pinned || input.active_reorderable,
            can_move_up: moves.can_move_up,
            can_move_down: moves.can_move_down,
            snooze_options: &snooze_options,
        },
    );
    ThreadRow {
        key: format!("thread:{}", thread.id),
        id: thread.id.clone(),
        title: thread.title.clone(),
        project_id: thread.project.clone(),
        project_title: None,
        branch: thread.branch.clone(),
        variant: item.variant,
        snoozed: item.snoozed,
        pinned: item.pinned,
        selected: Some(thread.id.as_str()) == input.selected,
        is_last: item.is_last,
        status,
        status_label: status
            .label()
            .or(unread.then_some("Done"))
            .map(String::from),
        unread,
        time_label,
        snooze_wake_label,
        snooze_gate_expires_at_ms: snooze_gate_expiry_ms(thread, input.now_ms),
        has_queued_messages: queued.contains(&thread.id),
        can_move_up: moves.can_move_up,
        can_move_down: moves.can_move_down,
        show_trailing_divider: false,
        provider_instances: thread.provider_stack(),
        search_snippet: None,
        swipe_primary: swipe.primary.into(),
        swipe_secondary: swipe.secondary.map(Into::into),
        snooze_options,
        menu,
    }
}

fn count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// The flat list: active cards, unsent new threads, then each shelf header
/// with its visible rows. Unsent threads wait rather than ask, so they sit
/// after the inbox and before parked work.
pub fn build_list_items(input: ListItemsInput) -> Vec<ThreadListItem> {
    let layout = input.layout;
    let queued = input.queued.unwrap_or(&NO_IDS);
    let disabled = input.shelf_preferences_loading;
    let snoozed_end = layout
        .settled_shelf_header_index
        .unwrap_or(layout.items.len());
    let working_end = layout.snoozed_shelf_header_index.unwrap_or(snoozed_end);
    let active_end = layout.working_shelf_header_index.unwrap_or(working_end);
    let rows: Vec<ThreadListItem> = layout
        .items
        .iter()
        .map(|item| ThreadListItem::Thread {
            row: thread_row(item, &input, queued),
        })
        .collect();
    let mut rows = rows.into_iter();
    let mut result: Vec<ThreadListItem> = rows.by_ref().take(active_end).collect();
    result.extend(
        input
            .pending_tasks
            .into_iter()
            .enumerate()
            .map(|(index, task)| ThreadListItem::PendingTask {
                task: PendingTaskRow {
                    show_pending_divider: index == 0,
                    show_trailing_divider: false,
                    ..task
                },
            }),
    );
    let shelf = |count_of: usize, expanded: bool| ShelfHeader {
        count: count(count_of),
        expanded,
        disabled,
    };
    if let Some(index) = layout.working_shelf_header_index
        && layout.working_count > 0
    {
        result.push(ThreadListItem::WorkingShelf {
            shelf: shelf(layout.working_count, input.working_shelf_expanded),
        });
        result.extend(rows.by_ref().take(working_end - index));
    }
    if let Some(index) = layout.snoozed_shelf_header_index
        && layout.snoozed_count > 0
    {
        result.push(ThreadListItem::SnoozedShelf {
            shelf: shelf(layout.snoozed_count, input.snoozed_shelf_expanded),
        });
        result.extend(rows.by_ref().take(snoozed_end - index));
    }
    if layout.settled_shelf_header_index.is_some() && layout.settled_count > 0 {
        result.push(ThreadListItem::SettledShelf {
            shelf: shelf(layout.settled_count, input.settled_shelf_expanded),
        });
        result.extend(rows);
    }
    // Hairlines depend on the final neighbour, so they follow the splice.
    let divided: Vec<bool> = (0..result.len())
        .map(|index| match result.get(index + 1) {
            Some(ThreadListItem::Thread { .. }) => true,
            Some(ThreadListItem::PendingTask { task }) => !task.show_pending_divider,
            _ => false,
        })
        .collect();
    for (item, divided) in result.iter_mut().zip(divided) {
        match item {
            ThreadListItem::Thread { row } => row.show_trailing_divider = divided,
            ThreadListItem::PendingTask { task } => task.show_trailing_divider = divided,
            _ => {}
        }
    }
    result
}

/// List preferences the native shell keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadListOptions {
    pub working_shelf_enabled: bool,
    pub working_shelf_expanded: bool,
    pub snoozed_shelf_expanded: bool,
    pub settled_shelf_expanded: bool,
    /// Starts at `SETTLED_INITIAL_COUNT`, grows by `SETTLED_PAGE_COUNT` on
    /// "Show more" and resets when the project or search changes.
    pub settled_limit: u32,
    pub shelf_preferences_loading: bool,
    pub timestamp_format: TimestampFormat,
}
impl Default for ThreadListOptions {
    fn default() -> Self {
        Self {
            working_shelf_enabled: false,
            working_shelf_expanded: false,
            snoozed_shelf_expanded: false,
            settled_shelf_expanded: true,
            settled_limit: SETTLED_INITIAL_COUNT,
            shelf_preferences_loading: false,
            timestamp_format: TimestampFormat::default(),
        }
    }
}

/// Device state the list reads that the snapshot does not carry: the inbox
/// returns this device observed, and a reorder waiting for the Host.
#[derive(Debug, Clone, Copy, Default)]
pub struct ThreadListHolds<'a> {
    pub inbox_returns: Option<&'a InboxReturns>,
    pub pending_order: Option<&'a PendingThreadOrder>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadListView {
    pub items: Vec<ThreadListItem>,
    /// Settled rows behind "Show more".
    pub hidden_settled_count: u32,
    /// Rebuild at this time so a woken thread reappears at once.
    pub next_snooze_wake_at_ms: Option<i64>,
    /// Anything to list before filters; otherwise the empty state shows.
    pub has_threads: bool,
}

/// Threads with a message the device sent and the Host has not folded.
pub fn queued_threads(snapshot: &Snapshot) -> BTreeSet<String> {
    snapshot
        .outbox
        .entries
        .iter()
        .filter(|entry| {
            matches!(&entry.request, Request::Dispatch(dispatch)
                if matches!(dispatch.command, Command::Send(_)))
        })
        .map(|entry| entry.thread.to_string())
        .collect()
}

fn launch_branch(workspace: &WorkspaceStrategy) -> Option<String> {
    match workspace {
        WorkspaceStrategy::Root { branch }
        | WorkspaceStrategy::ExistingWorktree { branch, .. }
        | WorkspaceStrategy::Worktree { branch, .. } => branch.clone(),
    }
}

/// New threads in the outbox the shell does not list yet, newest first.
pub fn pending_tasks(snapshot: &Snapshot, listed: &BTreeSet<&str>) -> Vec<PendingTaskRow> {
    let mut tasks: Vec<PendingTaskRow> = snapshot
        .outbox
        .pending_launches()
        .filter(|entry| !listed.contains(entry.thread.as_str()))
        .filter_map(|entry| match &entry.request {
            Request::Launch(launch) => Some(PendingTaskRow {
                key: format!("pending-task:{}", entry.id),
                command_id: entry.id.to_string(),
                thread_id: entry.thread.to_string(),
                project_id: launch.project_id.clone(),
                title: launch.title.clone(),
                branch: launch_branch(&launch.workspace),
                created_at_ms: entry.created_at.millis(),
                show_pending_divider: false,
                show_trailing_divider: false,
            }),
            Request::Dispatch(_) => None,
        })
        .collect();
    tasks.sort_by(|left, right| {
        right
            .created_at_ms
            .cmp(&left.created_at_ms)
            .then_with(|| left.key.cmp(&right.key))
    });
    tasks
}

/// The Home list and iPad sidebar, with snooze menus in the local time zone.
pub fn thread_list(
    snapshot: &Snapshot,
    now_ms: i64,
    options: &ThreadListOptions,
    holds: ThreadListHolds,
) -> ThreadListView {
    let now = Local
        .timestamp_millis_opt(now_ms)
        .single()
        .unwrap_or_default();
    thread_list_at(snapshot, &now, options, holds)
}

pub fn thread_list_at<Tz: TimeZone>(
    snapshot: &Snapshot,
    now: &DateTime<Tz>,
    options: &ThreadListOptions,
    holds: ThreadListHolds,
) -> ThreadListView {
    let now_ms = now.timestamp_millis();
    let shell = snapshot.shell_view();
    let summaries: Vec<ThreadSummary> = shell
        .iter()
        .flat_map(|shell| &shell.threads)
        .map(ThreadSummary::from_shell)
        .collect();
    let queued = queued_threads(snapshot);
    let listed: BTreeSet<&str> = summaries.iter().map(|thread| thread.id.as_str()).collect();
    let all_tasks = pending_tasks(snapshot, &listed);
    let has_threads =
        summaries.iter().any(|thread| thread.archived_at.is_none()) || !all_tasks.is_empty();
    let search = snapshot.search.trim().to_lowercase();
    let project = snapshot.selected_project.as_deref();
    let tasks = all_tasks
        .into_iter()
        .filter(|task| project.is_none_or(|project| task.project_id == project))
        .filter(|task| search.is_empty() || task.title.to_lowercase().contains(&search))
        .collect();
    let matched: BTreeSet<String> = if search.is_empty() {
        BTreeSet::new()
    } else {
        snapshot
            .search_matches
            .iter()
            .map(|found| found.thread_id.to_string())
            .collect()
    };
    let selected = snapshot.selected_thread.as_ref().map(|id| id.as_str());
    let pending = holds
        .pending_order
        .and_then(|pending| pending.refresh(&summaries, now_ms, &queued));
    let availability_of = |section| {
        let ordered = ordered_section(&summaries, section, pending.as_ref(), now_ms, &queued);
        move_availability(&ordered, Some(&summaries), section, pending.as_ref())
    };
    let mut availability = availability_of(OrderSection::Pinned);
    if !options.working_shelf_enabled {
        availability.extend(availability_of(OrderSection::Active));
    }
    let layout = build_thread_list_layout(&ThreadListInput {
        threads: &summaries,
        now_ms,
        pending_order: pending.as_ref(),
        project,
        search_query: &snapshot.search,
        matched: Some(&matched),
        queued: Some(&queued),
        selected,
        settled_limit: Some(options.settled_limit as usize),
        working_shelf_enabled: options.working_shelf_enabled,
        working_shelf_expanded: options.working_shelf_expanded,
        inbox_returns: holds.inbox_returns,
        snoozed_shelf_expanded: options.snoozed_shelf_expanded,
        settled_shelf_expanded: options.settled_shelf_expanded,
    });
    let presets = resolve_snooze_presets(now, options.timestamp_format);
    let mut items = build_list_items(ListItemsInput {
        layout: &layout,
        pending_tasks: tasks,
        now_ms,
        wake_label_now_ms: now_ms - now_ms.rem_euclid(60_000),
        snooze_presets: &presets,
        queued: Some(&queued),
        move_availability: Some(&availability),
        selected,
        active_reorderable: !options.working_shelf_enabled,
        working_shelf_expanded: options.working_shelf_expanded,
        snoozed_shelf_expanded: options.snoozed_shelf_expanded,
        settled_shelf_expanded: options.settled_shelf_expanded,
        shelf_preferences_loading: options.shelf_preferences_loading,
    });
    let projects: BTreeMap<&str, &str> = shell
        .iter()
        .flat_map(|shell| &shell.projects)
        .map(|project| (project.id.as_str(), project.name.as_str()))
        .collect();
    let snippets: BTreeMap<String, &str> = if search.is_empty() {
        BTreeMap::new()
    } else {
        snapshot
            .search_matches
            .iter()
            .map(|found| (found.thread_id.to_string(), found.snippet.as_str()))
            .collect()
    };
    for item in &mut items {
        if let ThreadListItem::Thread { row } = item {
            row.project_title = projects
                .get(row.project_id.as_str())
                .map(|name| name.to_string());
            row.search_snippet = snippets.get(&row.id).map(|snippet| snippet.to_string());
        }
    }
    ThreadListView {
        items,
        hidden_settled_count: count(layout.hidden_settled_count),
        next_snooze_wake_at_ms: layout.next_snooze_wake_at,
        has_threads,
    }
}

#[cfg(test)]
mod tests;
