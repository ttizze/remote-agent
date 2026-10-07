//! Desktop sidebar rules: shelves and drag and drop across them, row status,
//! recede and labels, search, bulk menus, project order and traversal.
use crate::view::collation::locale_compare;
use crate::view::snooze::effective_snoozed;
pub use crate::view::thread_order::DropSection;
use crate::view::thread_sort::{
    OrderAssignment, ThreadSortOrder, plan_pinned_reorder, sort_threads, thread_sort_timestamp,
};
use crate::view::thread_summary::{
    RuntimeStatus, SettledOverride, ThreadListStatus, ThreadSummary,
    background_work_holds_completion,
};
use agent_domain::InteractionMode;
use std::collections::{BTreeMap, BTreeSet};

/// Visible rows kept warm in the thread cache.
pub const SIDEBAR_THREAD_PREWARM_LIMIT: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ParkAction {
    Settle,
    Snooze,
}

/// Parking the open thread moves forward once the thread actually parked.
pub fn should_navigate_after_thread_park(
    thread_id: &str,
    current: Option<&str>,
    action: ParkAction,
    now_ms: i64,
    thread: Option<&ThreadSummary>,
) -> bool {
    let Some(thread) = thread else {
        return false;
    };
    current == Some(thread_id)
        && match action {
            ParkAction::Settle => thread.settled_override == Some(SettledOverride::Settled),
            ParkAction::Snooze => effective_snoozed(thread, now_ms),
        }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct RowAccessibility {
    pub label: String,
    pub current_page: bool,
}

/// The title leads; row actions stay separate controls.
pub fn sidebar_row_accessibility(
    title: &str,
    status_label: Option<&str>,
    project_name: Option<&str>,
    active: bool,
) -> RowAccessibility {
    RowAccessibility {
        label: [Some(title), status_label, project_name]
            .into_iter()
            .flatten()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        current_page: active,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SidebarSection {
    Pinned,
    Active,
    Working,
    Snoozed,
    Settled,
}

/// Snooze wins until its wake boundary; settlement then wins over a stale pin.
pub fn resolve_sidebar_thread_section(
    snoozed: bool,
    settled: bool,
    pinned: bool,
) -> SidebarSection {
    if snoozed {
        SidebarSection::Snoozed
    } else if settled {
        SidebarSection::Settled
    } else if pinned {
        SidebarSection::Pinned
    } else {
        SidebarSection::Active
    }
}

/// Structural entries of the sortable list. The pinned header and divider and
/// the placeholders take no space at rest; they are drop slots while dragging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SidebarListMarker {
    PinnedHeader,
    ActivePlaceholder,
    SettledPlaceholder,
    PinnedDivider,
    WorkingHeader,
    SnoozedHeader,
    SettledHeader,
}

impl SidebarListMarker {
    fn name(self) -> &'static str {
        match self {
            Self::PinnedHeader => "pinned-header",
            Self::ActivePlaceholder => "active-placeholder",
            Self::SettledPlaceholder => "settled-placeholder",
            Self::PinnedDivider => "pinned-divider",
            Self::WorkingHeader => "working-header",
            Self::SnoozedHeader => "snoozed-header",
            Self::SettledHeader => "settled-header",
        }
    }

    fn is_shelf_header(self) -> bool {
        matches!(
            self,
            Self::WorkingHeader | Self::SnoozedHeader | Self::SettledHeader
        )
    }
}

/// Marker ids carry a prefix thread ids never use.
pub fn sidebar_marker_id(marker: SidebarListMarker) -> String {
    format!("sidebar-marker-{}", marker.name())
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SidebarListItem {
    Thread {
        key: String,
        section: SidebarSection,
    },
    Marker {
        marker: SidebarListMarker,
    },
}

impl SidebarListItem {
    pub fn id(&self) -> String {
        match self {
            Self::Thread { key, .. } => key.clone(),
            Self::Marker { marker } => sidebar_marker_id(*marker),
        }
    }
}

/// The section of a slot, read off the markers above it.
fn section_at_slot(items: &[SidebarListItem], index: usize) -> SidebarSection {
    let mut section = SidebarSection::Pinned;
    for item in items.iter().take(index) {
        if let SidebarListItem::Marker { marker } = item {
            section = match marker {
                SidebarListMarker::PinnedDivider => SidebarSection::Active,
                SidebarListMarker::WorkingHeader => SidebarSection::Working,
                SidebarListMarker::SnoozedHeader => SidebarSection::Snoozed,
                SidebarListMarker::SettledHeader => SidebarSection::Settled,
                _ => section,
            };
        }
    }
    section
}

impl From<DropSection> for SidebarSection {
    fn from(section: DropSection) -> Self {
        match section {
            DropSection::Pinned => Self::Pinned,
            DropSection::Active => Self::Active,
            DropSection::Settled => Self::Settled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SidebarDropTarget {
    pub section: DropSection,
    pub pinned_order: Vec<String>,
    pub active_order: Vec<String>,
}

/// The destination section and the pinned and active orders after moving
/// `active_key` onto `over_id`.
pub fn resolve_sidebar_drop_target(
    items: &[SidebarListItem],
    active_key: &str,
    over_id: &str,
) -> Option<SidebarDropTarget> {
    let active = items.iter().position(|item| item.id() == active_key)?;
    let over = items.iter().position(|item| item.id() == over_id)?;
    if !matches!(items[active], SidebarListItem::Thread { .. }) {
        return None;
    }
    let mut moved: Vec<_> = items.to_vec();
    let lifted = moved.remove(active);
    moved.insert(over.min(moved.len()), lifted);
    let section = match section_at_slot(&moved, over) {
        SidebarSection::Pinned => DropSection::Pinned,
        SidebarSection::Active => DropSection::Active,
        SidebarSection::Settled => DropSection::Settled,
        SidebarSection::Working | SidebarSection::Snoozed => return None,
    };
    let mut pinned_order = vec![];
    let mut active_order = vec![];
    let mut in_pinned = true;
    for item in &moved {
        match item {
            SidebarListItem::Marker { marker } if marker.is_shelf_header() => break,
            SidebarListItem::Marker {
                marker: SidebarListMarker::PinnedDivider,
            } => in_pinned = false,
            SidebarListItem::Marker { .. } => {}
            SidebarListItem::Thread { key, .. } if in_pinned => pinned_order.push(key.clone()),
            SidebarListItem::Thread { key, .. } => active_order.push(key.clone()),
        }
    }
    Some(SidebarDropTarget {
        section,
        pinned_order,
        active_order,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SidebarThreadDropPlan {
    None,
    /// Within the pinned block: key writes only.
    ReorderPinned {
        order: Vec<String>,
        assignments: Vec<OrderAssignment>,
    },
    /// Into the pinned block. A fresh pin takes `order_key` with the pin;
    /// `extra_assignments` follow, including the moved row when it was already
    /// pinned beneath a snooze.
    Pin {
        order: Vec<String>,
        order_key: Option<String>,
        extra_assignments: Vec<OrderAssignment>,
    },
    MoveActive {
        /// `None` when the inbox is time-ordered: the drop has no placement.
        order: Option<Vec<String>>,
        assignments: Vec<OrderAssignment>,
        unpin: bool,
        unsettle: bool,
        unsnooze: bool,
    },
    Settle,
}

/// The badge on a lifted row: what dropping in `to` does to a thread from
/// `from`. `None` inside one section and over the Working and Snoozed shelves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SidebarDropVerb {
    Pin,
    Unpin,
    Settle,
    Unsettle,
    Wake,
}

pub fn resolve_sidebar_drop_verb(
    from: SidebarSection,
    to: Option<SidebarSection>,
) -> Option<SidebarDropVerb> {
    let to = to?;
    if to == from || matches!(to, SidebarSection::Working | SidebarSection::Snoozed) {
        return None;
    }
    Some(match (to, from) {
        (SidebarSection::Pinned, _) => SidebarDropVerb::Pin,
        (SidebarSection::Settled, _) => SidebarDropVerb::Settle,
        (_, SidebarSection::Pinned) => SidebarDropVerb::Unpin,
        (_, SidebarSection::Settled) => SidebarDropVerb::Unsettle,
        _ => SidebarDropVerb::Wake,
    })
}

/// Eligible rows between the pressed action and the pointer, in list order.
pub fn resolve_sidebar_sweep_keys(
    ordered: &[String],
    origin: &str,
    target: &str,
    can_apply: impl Fn(&str) -> bool,
) -> Vec<String> {
    let (Some(origin), Some(target)) = (
        ordered.iter().position(|key| key == origin),
        ordered.iter().position(|key| key == target),
    ) else {
        return vec![];
    };
    ordered[origin.min(target)..=origin.max(target)]
        .iter()
        .filter(|key| can_apply(key))
        .cloned()
        .collect()
}

pub struct SidebarDropInput<'a> {
    pub active_key: &'a str,
    pub active_section: SidebarSection,
    /// Defaults to the section; snoozed threads can keep a pin beneath.
    pub active_pinned: Option<bool>,
    /// Defaults to the section; snoozed threads can stay settled beneath.
    pub active_settled: Option<bool>,
    pub target: &'a SidebarDropTarget,
    /// Every pinned row in displayed order before the drop.
    pub pinned_order: &'a [String],
    pub pinned_keys: &'a BTreeMap<String, Option<String>>,
    pub active_order: &'a [String],
    pub active_keys: &'a BTreeMap<String, Option<String>>,
    /// The Working section (beta) orders the inbox by time, so drops into it
    /// only change lifecycle.
    pub active_time_ordered: bool,
}

pub fn plan_sidebar_thread_drop(input: &SidebarDropInput) -> SidebarThreadDropPlan {
    let section = input.active_section;
    let pinned = input
        .active_pinned
        .unwrap_or(section == SidebarSection::Pinned);
    let settled = input
        .active_settled
        .unwrap_or(section == SidebarSection::Settled);
    match input.target.section {
        DropSection::Active => {
            if input.active_time_ordered {
                if section == SidebarSection::Active {
                    return SidebarThreadDropPlan::None;
                }
                return SidebarThreadDropPlan::MoveActive {
                    order: None,
                    assignments: vec![],
                    unpin: pinned,
                    unsettle: settled,
                    unsnooze: section == SidebarSection::Snoozed,
                };
            }
            let order = &input.target.active_order;
            if section == SidebarSection::Active && order.as_slice() == input.active_order {
                return SidebarThreadDropPlan::None;
            }
            SidebarThreadDropPlan::MoveActive {
                order: Some(order.clone()),
                assignments: plan_pinned_reorder(order, input.active_keys, input.active_key),
                unpin: pinned,
                unsettle: settled,
                unsnooze: section == SidebarSection::Snoozed,
            }
        }
        DropSection::Settled if section == SidebarSection::Settled => SidebarThreadDropPlan::None,
        DropSection::Settled => SidebarThreadDropPlan::Settle,
        DropSection::Pinned => {
            let order = &input.target.pinned_order;
            if section == SidebarSection::Pinned && order.as_slice() == input.pinned_order {
                return SidebarThreadDropPlan::None;
            }
            let assignments = plan_pinned_reorder(order, input.pinned_keys, input.active_key);
            if section == SidebarSection::Pinned {
                return if assignments.is_empty() {
                    SidebarThreadDropPlan::None
                } else {
                    SidebarThreadDropPlan::ReorderPinned {
                        order: order.clone(),
                        assignments,
                    }
                };
            }
            SidebarThreadDropPlan::Pin {
                order: order.clone(),
                order_key: assignments
                    .iter()
                    .find(|assignment| assignment.id == input.active_key)
                    .map(|assignment| assignment.order_key.clone()),
                extra_assignments: assignments
                    .into_iter()
                    .filter(|assignment| pinned || assignment.id != input.active_key)
                    .collect(),
            }
        }
    }
}

/// A drop's lifecycle fields before the destination sorts, following the
/// Host's re-entry rules so the preview stays put when its events arrive.
pub fn apply_sidebar_thread_drop(
    thread: &ThreadSummary,
    section: DropSection,
    now_ms: i64,
    order_key: Option<&str>,
) -> ThreadSummary {
    let was_settled = thread.settled_override == Some(SettledOverride::Settled);
    let mut next = ThreadSummary {
        snoozed_at: None,
        snoozed_until: None,
        ..thread.clone()
    };
    if section == DropSection::Settled {
        next.pinned_at = None;
        next.pin_order_key = None;
        next.active_order_key = None;
        next.settled_override = Some(SettledOverride::Settled);
        next.settled_at = Some(if was_settled {
            thread.settled_at.unwrap_or(now_ms)
        } else {
            now_ms
        });
        next.unsettled_at = None;
        return next;
    }
    if was_settled {
        next.settled_override = Some(SettledOverride::Active);
        next.settled_at = None;
        next.unsettled_at = Some(now_ms);
    }
    let into_pins = section == DropSection::Pinned;
    next.pinned_at = into_pins.then(|| thread.pinned_at.unwrap_or(now_ms));
    next.pin_order_key = into_pins
        .then(|| order_key.map(String::from).or(thread.pin_order_key.clone()))
        .flatten();
    if section == DropSection::Active
        && let Some(key) = order_key
    {
        next.active_order_key = Some(key.into());
    }
    next
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SidebarMenuAction {
    MarkUnread,
    Archive,
    Delete,
    RegenerateTitle,
    Unpin,
    Settle,
    Snooze,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SidebarMenuItem {
    pub action: SidebarMenuAction,
    pub label: String,
    pub disabled: bool,
    pub destructive: bool,
}

impl SidebarMenuItem {
    pub fn new(action: SidebarMenuAction, label: String) -> Self {
        Self {
            action,
            label,
            disabled: false,
            destructive: false,
        }
    }
}

/// The per-project tree's bulk menu.
pub fn multi_select_thread_menu_items(
    count: usize,
    has_running_thread: bool,
) -> Vec<SidebarMenuItem> {
    vec![
        SidebarMenuItem::new(
            SidebarMenuAction::MarkUnread,
            format!("Mark unread ({count})"),
        ),
        SidebarMenuItem {
            disabled: has_running_thread,
            ..SidebarMenuItem::new(SidebarMenuAction::Archive, format!("Archive ({count})"))
        },
        SidebarMenuItem {
            destructive: true,
            ..SidebarMenuItem::new(SidebarMenuAction::Delete, format!("Delete ({count})"))
        },
    ]
}

/// The confirmation before deleting a multi-selection.
pub fn bulk_delete_confirmation(count: usize) -> String {
    format!(
        "Delete {count} thread{}?\nThis permanently clears conversation history for these threads.",
        if count == 1 { "" } else { "s" }
    )
}

/// Multi-selected rows and the row that anchors Shift+click ranges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SidebarSelection {
    pub selected: Vec<String>,
    pub anchor: Option<String>,
}

impl SidebarSelection {
    /// Mod+click: toggles the row; a newly added row becomes the anchor.
    pub fn toggle(&mut self, key: &str) {
        if let Some(index) = self.selected.iter().position(|selected| selected == key) {
            self.selected.remove(index);
        } else {
            self.selected.push(key.into());
            self.anchor = Some(key.into());
        }
    }

    /// Shift+click: adds every row between the anchor and `key` in `ordered`,
    /// keeping the anchor; without a usable anchor it adds `key` and anchors it.
    pub fn extend_to(&mut self, key: &str, ordered: &[String]) {
        let range = self.anchor.as_deref().and_then(|anchor| {
            let anchor = ordered.iter().position(|id| id == anchor)?;
            let target = ordered.iter().position(|id| id == key)?;
            Some(&ordered[anchor.min(target)..=anchor.max(target)])
        });
        let Some(range) = range else {
            if !self.selected.iter().any(|selected| selected == key) {
                self.selected.push(key.into());
            }
            self.anchor = Some(key.into());
            return;
        };
        for id in range {
            if !self.selected.contains(id) {
                self.selected.push(id.clone());
            }
        }
    }

    /// A plain click opens the row: the selection clears and the row anchors
    /// the next range.
    pub fn open(&mut self, key: &str) {
        self.selected.clear();
        self.anchor = Some(key.into());
    }
}

/// Counts only threads that can start a regeneration; a disabled progress
/// item while every one is already regenerating.
pub fn bulk_title_regeneration_menu_item(
    supported: usize,
    actionable: usize,
) -> Option<SidebarMenuItem> {
    if supported == 0 {
        return None;
    }
    if actionable == 0 {
        return Some(SidebarMenuItem {
            disabled: true,
            ..SidebarMenuItem::new(
                SidebarMenuAction::RegenerateTitle,
                format!("Regenerating… ({supported})"),
            )
        });
    }
    Some(SidebarMenuItem::new(
        SidebarMenuAction::RegenerateTitle,
        format!("Regenerate titles ({actionable})"),
    ))
}

/// Counts the pinned rows of a mixed selection; absent when none is pinned.
pub fn bulk_unpin_menu_item(pinned: usize) -> Option<SidebarMenuItem> {
    (pinned > 0)
        .then(|| SidebarMenuItem::new(SidebarMenuAction::Unpin, format!("Unpin ({pinned})")))
}

/// Unarchived top-level threads in the project scope; subagents live in their
/// parent's agents surface.
pub fn filter_sidebar_visible_threads<T: AsRef<ThreadSummary>>(
    threads: Vec<T>,
    scope: Option<&str>,
) -> Vec<T> {
    threads
        .into_iter()
        .filter(|thread| {
            let thread = thread.as_ref();
            thread.archived_at.is_none()
                && !thread.subagent
                && scope.is_none_or(|project| thread.project == project)
        })
        .collect()
}

pub fn sidebar_fork_parent_thread_id(thread: &ThreadSummary) -> Option<&str> {
    thread.forked.then_some(thread.parent.as_deref()).flatten()
}

/// Background work recedes unless it is open or selected; ready and approval
/// rows keep their unread and wake prominence.
pub fn should_recede_sidebar_thread(
    status: ThreadListStatus,
    unread: bool,
    woke: bool,
    active: bool,
    selected: bool,
) -> bool {
    if active || selected || status == ThreadListStatus::Input {
        return false;
    }
    match status {
        ThreadListStatus::Working | ThreadListStatus::Waiting => true,
        ThreadListStatus::Ready | ThreadListStatus::Approval => !unread && !woke,
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SidebarTopStatus {
    Approval,
    Done,
    Failed,
    Limited,
    Input,
    Waiting,
    Woke,
    Working,
}

impl SidebarTopStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Approval => "Approval",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Limited => "Limited",
            Self::Input => "Input",
            Self::Waiting => "Waiting",
            Self::Woke => "Woke",
            Self::Working => "Working",
        }
    }
}

pub fn resolve_sidebar_top_status(
    status: ThreadListStatus,
    unread: bool,
    woke: bool,
) -> Option<SidebarTopStatus> {
    Some(match status {
        ThreadListStatus::Working => SidebarTopStatus::Working,
        ThreadListStatus::Waiting => SidebarTopStatus::Waiting,
        ThreadListStatus::Approval => SidebarTopStatus::Approval,
        ThreadListStatus::Input => SidebarTopStatus::Input,
        ThreadListStatus::Failed => SidebarTopStatus::Failed,
        ThreadListStatus::Limited => SidebarTopStatus::Limited,
        ThreadListStatus::Ready if woke => SidebarTopStatus::Woke,
        ThreadListStatus::Ready if unread => SidebarTopStatus::Done,
        ThreadListStatus::Ready => return None,
    })
}

pub fn should_show_sidebar_duration(status: ThreadListStatus) -> bool {
    status == ThreadListStatus::Working
}

/// "42s", "5m", "1h 30m"; negative elapsed time reads "0s".
pub fn format_working_duration_label(elapsed_ms: i64) -> String {
    let seconds = elapsed_ms.max(0) / 1000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    format!("{}h {}m", minutes / 60, minutes % 60)
}

/// One project scope choice; `project_id: None` is "All projects".
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SidebarProjectScopeItem {
    pub project_id: Option<String>,
    pub label: String,
    pub selected: bool,
}

/// "All projects" heads the list while the query is empty and drops out
/// while filtering.
pub fn filter_sidebar_project_scope_items(
    items: &[SidebarProjectScopeItem],
    query: &str,
    matches: impl Fn(&SidebarProjectScopeItem, &str) -> bool,
) -> Vec<SidebarProjectScopeItem> {
    let query = query.trim();
    if query.is_empty() {
        return items.to_vec();
    }
    items
        .iter()
        .filter(|item| item.project_id.is_some() && matches(item, query))
        .cloned()
        .collect()
}

/// Case-insensitive label match for the project scope search.
pub fn project_scope_label_matches(item: &SidebarProjectScopeItem, query: &str) -> bool {
    item.label.to_lowercase().contains(&query.to_lowercase())
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProjectScopeMenuState {
    pub open: bool,
    pub query: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ProjectScopeMenuAction {
    QueryChanged { query: String },
    OpenChanged { open: bool },
    ProjectSettingsOpened,
}

/// Closing the menu, either way, clears its query.
pub fn reduce_project_scope_menu_state(
    state: ProjectScopeMenuState,
    action: ProjectScopeMenuAction,
) -> ProjectScopeMenuState {
    match action {
        ProjectScopeMenuAction::QueryChanged { query } => ProjectScopeMenuState { query, ..state },
        ProjectScopeMenuAction::OpenChanged { open } => ProjectScopeMenuState {
            open,
            query: String::new(),
        },
        ProjectScopeMenuAction::ProjectSettingsOpened => ProjectScopeMenuState::default(),
    }
}

/// The per-project tree's pill labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ThreadStatusPill {
    Working,
    Connecting,
    Completed,
    PendingApproval,
    AwaitingInput,
    Waiting,
    PlanReady,
}

impl ThreadStatusPill {
    pub fn label(self) -> &'static str {
        match self {
            Self::Working => "Working",
            Self::Connecting => "Connecting",
            Self::Completed => "Completed",
            Self::PendingApproval => "Pending Approval",
            Self::AwaitingInput => "Awaiting Input",
            Self::Waiting => "Waiting",
            Self::PlanReady => "Plan Ready",
        }
    }

    pub fn pulse(self) -> bool {
        matches!(self, Self::Working | Self::Connecting)
    }

    /// Doubled so Waiting sits between active work and a ready plan.
    fn priority(self) -> u8 {
        match self {
            Self::PendingApproval => 10,
            Self::AwaitingInput => 8,
            Self::Working | Self::Connecting => 6,
            Self::Waiting => 5,
            Self::PlanReady => 4,
            Self::Completed => 2,
        }
    }
}

pub fn resolve_thread_status_pill(thread: &ThreadSummary) -> Option<ThreadStatusPill> {
    if thread.has_pending_approvals {
        return Some(ThreadStatusPill::PendingApproval);
    }
    if thread.has_pending_user_input {
        return Some(ThreadStatusPill::AwaitingInput);
    }
    match thread.runtime_status() {
        Some(RuntimeStatus::Running | RuntimeStatus::Waiting) => {
            return Some(ThreadStatusPill::Working);
        }
        Some(RuntimeStatus::Preparing | RuntimeStatus::Starting | RuntimeStatus::Queued) => {
            return Some(ThreadStatusPill::Connecting);
        }
        _ => {}
    }
    if background_work_holds_completion(&thread.pending_background) {
        return Some(ThreadStatusPill::Waiting);
    }
    if thread.interaction_mode == InteractionMode::Plan
        && thread.latest_run_settled()
        && thread.has_actionable_proposed_plan
    {
        return Some(ThreadStatusPill::PlanReady);
    }
    thread
        .has_unseen_completion()
        .then_some(ThreadStatusPill::Completed)
}

/// The most urgent pill across a project's threads; the first wins a tie.
pub fn resolve_project_status_indicator(
    statuses: &[Option<ThreadStatusPill>],
) -> Option<ThreadStatusPill> {
    statuses
        .iter()
        .flatten()
        .fold(None, |best, status| match best {
            Some(best) if best.priority() >= status.priority() => Some(best),
            _ => Some(*status),
        })
}

/// The top remaining thread of the deleted thread's project.
pub fn fallback_thread_after_delete(
    threads: &[ThreadSummary],
    deleted: &str,
    order: ThreadSortOrder,
    deleted_ids: &BTreeSet<String>,
) -> Option<String> {
    let project = &threads.iter().find(|thread| thread.id == deleted)?.project;
    let remaining: Vec<_> = threads
        .iter()
        .filter(|thread| {
            &thread.project == project && thread.id != deleted && !deleted_ids.contains(&thread.id)
        })
        .collect();
    sort_threads(remaining, order)
        .first()
        .map(|thread| thread.id.clone())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SidebarProjectSortOrder {
    #[default]
    UpdatedAt,
    CreatedAt,
    Manual,
}

/// A project as the sidebar orders it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarProjectInput {
    pub id: String,
    pub title: String,
    pub created_at: Option<i64>,
    pub updated_at: Option<i64>,
}

/// The newest thread stamp, or the project's own stamp when it has no threads;
/// `i64::MIN` when neither exists.
pub fn project_sort_timestamp(
    project: &SidebarProjectInput,
    threads: &[&ThreadSummary],
    order: ThreadSortOrder,
) -> i64 {
    if !threads.is_empty() {
        return threads
            .iter()
            .map(|thread| thread_sort_timestamp(thread, order))
            .fold(i64::MIN, i64::max);
    }
    match order {
        ThreadSortOrder::CreatedAt => project.created_at,
        ThreadSortOrder::UpdatedAt => project.updated_at.or(project.created_at),
    }
    .unwrap_or(i64::MIN)
}

/// Most recent activity first, then title and id; manual keeps the input.
pub fn sort_projects_for_sidebar<T: AsRef<ThreadSummary>>(
    projects: Vec<SidebarProjectInput>,
    threads: &[T],
    order: SidebarProjectSortOrder,
) -> Vec<SidebarProjectInput> {
    let thread_order = match order {
        SidebarProjectSortOrder::Manual => return projects,
        SidebarProjectSortOrder::UpdatedAt => ThreadSortOrder::UpdatedAt,
        SidebarProjectSortOrder::CreatedAt => ThreadSortOrder::CreatedAt,
    };
    let mut by_project: BTreeMap<&str, Vec<&ThreadSummary>> = BTreeMap::new();
    for thread in threads {
        let thread = thread.as_ref();
        by_project.entry(&thread.project).or_default().push(thread);
    }
    let mut keyed: Vec<_> = projects
        .into_iter()
        .map(|project| {
            let threads = by_project
                .get(project.id.as_str())
                .map_or(&[][..], Vec::as_slice);
            (
                project_sort_timestamp(&project, threads, thread_order),
                project,
            )
        })
        .collect();
    keyed.sort_by(|(left_ms, left), (right_ms, right)| {
        right_ms
            .cmp(left_ms)
            .then_with(|| locale_compare(&left.title, &right.title))
            .then_with(|| locale_compare(&left.id, &right.id))
    });
    keyed.into_iter().map(|(_, project)| project).collect()
}

/// Projects in sidebar order: hidden subagent and archived threads do not
/// count as activity.
pub fn sort_sidebar_project_groups<T: AsRef<ThreadSummary>>(
    projects: Vec<SidebarProjectInput>,
    threads: &[T],
    order: SidebarProjectSortOrder,
) -> Vec<SidebarProjectInput> {
    let visible = filter_sidebar_visible_threads(threads.iter().collect(), None);
    sort_projects_for_sidebar(projects, &visible, order)
}

/// Preferred ids first, each matching the first unused item that answers to
/// it; the rest keep their order.
pub fn order_items_by_preferred_ids<T, K: Ord>(
    items: Vec<T>,
    preferred: &[K],
    preference_ids: impl Fn(&T) -> Vec<K>,
) -> Vec<T> {
    if preferred.is_empty() {
        return items;
    }
    let mut indexes: BTreeMap<K, Vec<usize>> = BTreeMap::new();
    for (index, item) in items.iter().enumerate() {
        let ids: BTreeSet<K> = preference_ids(item).into_iter().collect();
        for id in ids {
            indexes.entry(id).or_default().push(index);
        }
    }
    let mut emitted = vec![false; items.len()];
    let mut order = vec![];
    for id in preferred {
        if let Some(index) = indexes
            .get(id)
            .and_then(|candidates| candidates.iter().find(|index| !emitted[**index]))
        {
            emitted[*index] = true;
            order.push(*index);
        }
    }
    order.extend((0..items.len()).filter(|index| !emitted[*index]));
    let mut slots: Vec<Option<T>> = items.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|index| slots[index].take())
        .collect()
}

pub fn sidebar_thread_ids_to_prewarm<T: Clone>(visible: &[T], limit: usize) -> Vec<T> {
    visible[..limit.min(visible.len())].to_vec()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum TraversalDirection {
    Previous,
    Next,
}

/// Without a current thread, traversal enters at the matching end.
pub fn resolve_adjacent_thread_id<'a>(
    ids: &'a [String],
    current: Option<&str>,
    direction: TraversalDirection,
) -> Option<&'a str> {
    let Some(current) = current else {
        return match direction {
            TraversalDirection::Previous => ids.last(),
            TraversalDirection::Next => ids.first(),
        }
        .map(String::as_str);
    };
    let index = ids.iter().position(|id| id == current)?;
    match direction {
        TraversalDirection::Previous => index.checked_sub(1).map(|index| ids[index].as_str()),
        TraversalDirection::Next => ids.get(index + 1).map(String::as_str),
    }
}

/// Shift creates in the current project; with one project there is nothing
/// to pick.
pub fn should_create_new_thread_in_current_project(shift: bool, project_count: usize) -> bool {
    shift || project_count <= 1
}

#[cfg(test)]
mod tests;
