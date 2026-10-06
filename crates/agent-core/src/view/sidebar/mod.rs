//! The desktop sidebar: unsent new-thread drafts, then one list of pinned,
//! active, Working, Snoozed and Settled threads with shelf headers, or search
//! results while a query is typed.
mod logic;

pub use logic::*;

use crate::state::Snapshot;
use crate::view::inbox::{
    InboxReturns, is_thread_working, sort_inbox_threads_by_return, sort_working_threads_by_send,
};
use crate::view::snooze::{can_snooze, effective_snoozed, snooze_wake_label, thread_woke_at};
use crate::view::thread_sort::{
    settled_thread_timestamp, sort_active_threads_by_order_key, sort_pinned_threads_by_order_key,
    sort_settled_threads,
};
use crate::view::thread_summary::{SettledOverride, ThreadSummary};
use agent_protocol::conversation::SearchSource;
use std::collections::{BTreeMap, BTreeSet};

/// Settled rows shown before the first "Show more".
pub const SETTLED_TAIL_INITIAL_COUNT: u32 = 10;
/// Settled rows each "Show more" adds.
pub const SETTLED_TAIL_PAGE_COUNT: u32 = 25;

/// Sidebar state the snapshot does not hold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SidebarOptions {
    /// The Working section (beta).
    pub working_section: bool,
    pub working_expanded: bool,
    pub snoozed_expanded: bool,
    pub settled_expanded: bool,
    /// "Show more" presses on the settled shelf; reset it when the project
    /// scope changes.
    pub settled_pages: u32,
    /// Multi-selected thread ids.
    pub selection: Vec<String>,
    pub project_sort_order: SidebarProjectSortOrder,
    /// Saved manual project order.
    pub project_order: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SidebarRowVariant {
    /// Pinned, active and Working rows.
    Card,
    /// Snoozed and settled rows.
    Slim,
}

/// The row's trailing slot at rest.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SidebarRowTrailing {
    /// `Woke` dismisses the wake when clicked; `duration` ticks while working.
    Status {
        status: SidebarTopStatus,
        label: String,
        duration: Option<String>,
    },
    /// A snoozed row's return time.
    WakesIn {
        label: String,
    },
    Time {
        label: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SidebarRowSurface {
    Active,
    Selected,
    /// Tinted for an unsent draft.
    Draft,
    Receded,
    Plain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SidebarTitleTone {
    /// Full foreground.
    Prominent,
    /// Card rows at rest.
    Normal,
    /// A failed card, a shade above normal.
    Failed,
    /// An unread slim row.
    Muted,
    /// A receded card.
    Secondary,
    /// Slim rows at rest and receded slim rows.
    SecondaryDim,
}

/// Hover actions in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SidebarRowAction {
    DiscardDraft,
    Snooze,
    Settle,
    Unsettle,
    Wake,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SidebarPullRequest {
    pub number: u64,
    pub repository: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SidebarThreadRow {
    pub id: String,
    pub title: String,
    pub section: SidebarSection,
    pub variant: SidebarRowVariant,
    pub project_id: String,
    pub project_name: Option<String>,
    pub branch: Option<String>,
    /// Shown as the worktree mark beside the branch.
    pub worktree_path: Option<String>,
    pub linked_pull_request: Option<SidebarPullRequest>,
    /// Provider instances, earlier owners first, the current one last.
    pub provider_stack: Vec<String>,
    /// Shows the pin, which unpins when clicked.
    pub pinned: bool,
    /// The open thread.
    pub active: bool,
    /// Multi-selected.
    pub selected: bool,
    pub status: SidebarThreadStatus,
    pub top_status: Option<SidebarTopStatus>,
    /// Where a working row's duration counts from.
    pub working_started_at: Option<i64>,
    pub trailing: SidebarRowTrailing,
    pub unread: bool,
    /// Set while the Woke mark shows; dismissing records it as the visit.
    pub woke_at: Option<i64>,
    pub recede: bool,
    /// Receded working rows fade as a whole.
    pub faded: bool,
    pub surface: SidebarRowSurface,
    pub title_tone: SidebarTitleTone,
    pub title_regenerating: bool,
    pub has_unsent_draft: bool,
    pub draggable: bool,
    pub can_snooze: bool,
    pub actions: Vec<SidebarRowAction>,
    pub accessibility_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SidebarShelfHeader {
    pub section: SidebarSection,
    /// "Working", or "Working (3)" while collapsed.
    pub label: String,
    pub count: u32,
    pub expanded: bool,
    /// The first shelf is pushed to the bottom of the sidebar.
    pub bottom_anchor: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
#[allow(
    clippy::large_enum_variant,
    reason = "exported records cannot be boxed"
)]
pub enum SidebarItem {
    /// The pinned header and divider and the empty-section placeholders:
    /// no height at rest, drop slots while dragging.
    Boundary {
        marker: SidebarListMarker,
    },
    Shelf {
        header: SidebarShelfHeader,
    },
    Thread {
        row: SidebarThreadRow,
    },
    /// More settled rows behind the open page.
    ShowMore {
        count: u32,
    },
}

/// An unsent new-thread draft: project, then the first line of the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SidebarDraftRow {
    pub draft_key: String,
    pub project_id: String,
    pub project_name: Option<String>,
    pub preview: String,
    pub accessibility_label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SidebarMatchSource {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SidebarSearchResult {
    pub id: String,
    pub title: String,
    pub project_id: String,
    pub project_name: Option<String>,
    pub time_label: String,
    pub active: bool,
    /// The Host's best message match, for content-only and title matches alike.
    pub snippet: Option<String>,
    pub snippet_source: Option<SidebarMatchSource>,
    pub accessibility_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SidebarSearchView {
    pub query: String,
    pub results: Vec<SidebarSearchResult>,
    pub empty_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SidebarEmptyState {
    /// Offers Add project.
    NoProjects,
    NoThreadsInProject {
        project_name: String,
    },
    NoThreads,
}

impl SidebarEmptyState {
    pub fn label(&self) -> String {
        match self {
            Self::NoProjects => "No projects yet".into(),
            Self::NoThreadsInProject { project_name } => {
                format!("No threads in {project_name} yet")
            }
            Self::NoThreads => "No threads yet".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SidebarView {
    /// Shown above the list, followed by a divider when present.
    pub drafts: Vec<SidebarDraftRow>,
    pub items: Vec<SidebarItem>,
    /// Replaces drafts and items while the query is not blank.
    pub search: Option<SidebarSearchView>,
    pub empty_state: Option<SidebarEmptyState>,
    /// "All projects", then projects in sidebar order.
    pub project_scope: Vec<SidebarProjectScopeItem>,
    /// The Working section orders the inbox by time; drops there only change
    /// lifecycle.
    pub inbox_time_ordered: bool,
}

/// Where parking the open thread moves.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ForwardNavigation {
    Thread { id: String },
    NewThread { project_id: String },
    Home,
}

impl SidebarView {
    pub fn rows(&self) -> impl Iterator<Item = &SidebarThreadRow> {
        self.items.iter().filter_map(|item| match item {
            SidebarItem::Thread { row } => Some(row),
            _ => None,
        })
    }

    /// Rendered rows in order: traversal, jump shortcuts and range selection.
    pub fn thread_ids(&self) -> Vec<String> {
        self.rows().map(|row| row.id.clone()).collect()
    }

    pub fn row(&self, id: &str) -> Option<&SidebarThreadRow> {
        self.rows().find(|row| row.id == id)
    }

    /// The sortable list a drag resolves against.
    pub fn list_items(&self) -> Vec<SidebarListItem> {
        self.items
            .iter()
            .filter_map(|item| match item {
                SidebarItem::Boundary { marker } => {
                    Some(SidebarListItem::Marker { marker: *marker })
                }
                SidebarItem::Shelf { header } => Some(SidebarListItem::Marker {
                    marker: match header.section {
                        SidebarSection::Working => SidebarListMarker::WorkingHeader,
                        SidebarSection::Snoozed => SidebarListMarker::SnoozedHeader,
                        _ => SidebarListMarker::SettledHeader,
                    },
                }),
                SidebarItem::Thread { row } => Some(SidebarListItem::Thread {
                    key: row.id.clone(),
                    section: row.section,
                }),
                SidebarItem::ShowMore { .. } => None,
            })
            .collect()
    }

    /// Rows a drag from `origin`'s action reaches; a sweep stays in its section.
    pub fn sweep_keys(&self, origin: &str, target: &str) -> Vec<String> {
        let Some(section) = self.row(origin).map(|row| row.section) else {
            return vec![];
        };
        resolve_sidebar_sweep_keys(&self.thread_ids(), origin, target, |key| {
            self.row(key).is_some_and(|row| row.section == section)
        })
    }

    /// The next card after the open thread, wrapping around, skipping parked
    /// rows and those parking with it; otherwise a new thread in its project.
    pub fn forward_target(
        &self,
        thread_id: &str,
        current: Option<&str>,
        co_parking: &[String],
    ) -> Option<ForwardNavigation> {
        if current != Some(thread_id) {
            return None;
        }
        let ids = self.thread_ids();
        let next = ids.iter().position(|id| id == thread_id).and_then(|index| {
            ids[index + 1..].iter().chain(&ids[..index]).find(|id| {
                self.row(id).is_some_and(|row| {
                    !matches!(
                        row.section,
                        SidebarSection::Settled | SidebarSection::Snoozed
                    )
                }) && !co_parking.contains(id)
            })
        });
        Some(match (next, self.row(thread_id)) {
            (Some(id), _) => ForwardNavigation::Thread { id: id.clone() },
            (None, Some(row)) => ForwardNavigation::NewThread {
                project_id: row.project_id.clone(),
            },
            (None, None) => ForwardNavigation::Home,
        })
    }

    /// The context menu for several rendered rows; empty when none of
    /// `selection` is rendered. Snooze takes the snooze presets as children.
    pub fn multi_select_menu(&self, selection: &[String]) -> Vec<SidebarMenuItem> {
        let rows: Vec<_> = self
            .rows()
            .filter(|row| selection.contains(&row.id))
            .collect();
        let count = rows.len();
        if count == 0 {
            return vec![];
        }
        let pinned = rows.iter().filter(|row| row.pinned).count();
        let regeneratable = rows.iter().filter(|row| !row.title_regenerating).count();
        let mut items: Vec<_> = bulk_unpin_menu_item(pinned).into_iter().collect();
        items.push(SidebarMenuItem::new(
            SidebarMenuAction::Settle,
            format!("Settle ({count})"),
        ));
        if rows.iter().all(|row| row.can_snooze) {
            items.push(SidebarMenuItem::new(
                SidebarMenuAction::Snooze,
                format!("Snooze ({count})"),
            ));
        }
        items.extend(bulk_title_regeneration_menu_item(count, regeneratable));
        items.push(SidebarMenuItem::new(
            SidebarMenuAction::MarkUnread,
            format!("Mark unread ({count})"),
        ));
        items.push(SidebarMenuItem {
            destructive: true,
            ..SidebarMenuItem::new(SidebarMenuAction::Delete, format!("Delete ({count})"))
        });
        items
    }
}

fn shell_threads(
    snapshot: &Snapshot,
) -> (Vec<ThreadSummary>, Vec<agent_protocol::models::Project>) {
    snapshot
        .shell_view()
        .map_or_else(Default::default, |shell| {
            (
                shell
                    .threads
                    .iter()
                    .map(ThreadSummary::from_shell)
                    .collect(),
                shell.projects.clone(),
            )
        })
}

/// Records which threads left the Working section since the last call; call
/// before [`sidebar`] on every rebuild, with the beta's current state.
pub fn observe_inbox_returns(
    snapshot: &Snapshot,
    now_ms: i64,
    working_section: bool,
    returns: &mut InboxReturns,
) {
    let (threads, _) = shell_threads(snapshot);
    returns.observe(working_section.then_some(threads.as_slice()), now_ms);
}

struct Shelves {
    pinned: Vec<ThreadSummary>,
    active: Vec<ThreadSummary>,
    working: Vec<ThreadSummary>,
    snoozed: Vec<ThreadSummary>,
    settled: Vec<ThreadSummary>,
}

impl Shelves {
    fn total(&self) -> usize {
        self.pinned.len()
            + self.active.len()
            + self.working.len()
            + self.snoozed.len()
            + self.settled.len()
    }
}

fn shelve(
    visible: Vec<ThreadSummary>,
    now_ms: i64,
    working_section: bool,
    returns: &InboxReturns,
) -> Shelves {
    let mut shelves = Shelves {
        pinned: vec![],
        active: vec![],
        working: vec![],
        snoozed: vec![],
        settled: vec![],
    };
    for thread in visible {
        let section = resolve_sidebar_thread_section(
            effective_snoozed(&thread, now_ms),
            thread.settled_override == Some(SettledOverride::Settled),
            thread.pinned_at.is_some(),
        );
        match section {
            SidebarSection::Snoozed => shelves.snoozed.push(thread),
            SidebarSection::Settled => shelves.settled.push(thread),
            SidebarSection::Pinned => shelves.pinned.push(thread),
            _ if working_section && is_thread_working(&thread) => shelves.working.push(thread),
            _ => shelves.active.push(thread),
        }
    }
    shelves.pinned = sort_pinned_threads_by_order_key(shelves.pinned);
    shelves.active = if working_section {
        sort_inbox_threads_by_return(shelves.active, returns)
    } else {
        sort_active_threads_by_order_key(shelves.active)
    };
    shelves.working = sort_working_threads_by_send(shelves.working);
    shelves
        .snoozed
        .sort_by_key(|thread| thread.snoozed_until.unwrap_or(0));
    shelves.settled = sort_settled_threads(shelves.settled);
    shelves
}

/// A collapsed shelf keeps only the open thread's row.
fn collapsed<'a>(
    threads: &'a [ThreadSummary],
    expanded: bool,
    route: Option<&str>,
) -> Vec<&'a ThreadSummary> {
    threads
        .iter()
        .filter(|thread| expanded || Some(thread.id.as_str()) == route)
        .collect()
}

struct RowContext<'a> {
    now_ms: i64,
    route: Option<&'a str>,
    selection: &'a [String],
    project_names: &'a BTreeMap<String, String>,
    snapshot: &'a Snapshot,
}

fn thread_time_label(thread: &ThreadSummary, now_ms: i64) -> String {
    crate::view::time::compact_relative_time_label(
        thread.latest_user_message_at.unwrap_or(thread.updated_at),
        now_ms,
    )
}

fn thread_row(
    thread: &ThreadSummary,
    section: SidebarSection,
    context: &RowContext,
) -> SidebarThreadRow {
    let now_ms = context.now_ms;
    let active = context.route == Some(thread.id.as_str());
    let selected = context.selection.contains(&thread.id);
    let variant = match section {
        SidebarSection::Snoozed | SidebarSection::Settled => SidebarRowVariant::Slim,
        _ => SidebarRowVariant::Card,
    };
    let has_unsent_draft = !active
        && context
            .snapshot
            .drafts
            .get(&thread.id)
            .is_some_and(|draft| !draft.is_empty());
    let unread = thread.has_unseen_completion();
    let status = resolve_sidebar_thread_status(thread);
    let woke_at = thread_woke_at(thread, now_ms).filter(|woke| {
        thread.last_visited_at.is_none_or(|visited| visited < *woke)
            && thread.settled_override != Some(SettledOverride::Settled)
    });
    let woke = woke_at.is_some();
    let recede = should_recede_sidebar_thread(status, unread, woke, active, selected);
    let top_status = resolve_sidebar_top_status(status, unread, woke);
    let working_started_at = should_show_sidebar_duration(status)
        .then(|| thread.working_started_at())
        .flatten();
    let status_slot = |status: SidebarTopStatus| SidebarRowTrailing::Status {
        status,
        label: status.label().into(),
        duration: (status == SidebarTopStatus::Working)
            .then(|| {
                working_started_at.map(|started| format_working_duration_label(now_ms - started))
            })
            .flatten(),
    };
    let can_snooze = can_snooze(thread, now_ms);
    let (trailing, actions, title_tone) = match section {
        SidebarSection::Snoozed | SidebarSection::Settled => {
            let wakes_in = (section == SidebarSection::Snoozed)
                .then(|| {
                    thread
                        .snoozed_until
                        .map(|until| snooze_wake_label(until, now_ms))
                })
                .flatten();
            let trailing = match wakes_in {
                Some(label) => SidebarRowTrailing::WakesIn { label },
                None if woke => status_slot(SidebarTopStatus::Woke),
                None => SidebarRowTrailing::Time {
                    label: if section == SidebarSection::Settled {
                        crate::view::time::compact_relative_time_label(
                            settled_thread_timestamp(thread),
                            now_ms,
                        )
                    } else {
                        thread_time_label(thread, now_ms)
                    },
                },
            };
            let action = if section == SidebarSection::Snoozed {
                SidebarRowAction::Wake
            } else {
                SidebarRowAction::Unsettle
            };
            let tone = if recede {
                SidebarTitleTone::SecondaryDim
            } else if active || woke || status == SidebarThreadStatus::Input {
                SidebarTitleTone::Prominent
            } else if unread {
                SidebarTitleTone::Muted
            } else {
                SidebarTitleTone::SecondaryDim
            };
            (trailing, vec![action], tone)
        }
        _ => {
            let trailing = top_status.map_or_else(
                || SidebarRowTrailing::Time {
                    label: thread_time_label(thread, now_ms),
                },
                status_slot,
            );
            let actions = [
                has_unsent_draft.then_some(SidebarRowAction::DiscardDraft),
                can_snooze.then_some(SidebarRowAction::Snooze),
                Some(SidebarRowAction::Settle),
            ]
            .into_iter()
            .flatten()
            .collect();
            let tone = if recede {
                SidebarTitleTone::Secondary
            } else if unread || woke || status == SidebarThreadStatus::Input {
                SidebarTitleTone::Prominent
            } else if status == SidebarThreadStatus::Failed {
                SidebarTitleTone::Failed
            } else {
                SidebarTitleTone::Normal
            };
            (trailing, actions, tone)
        }
    };
    let surface = if active {
        SidebarRowSurface::Active
    } else if selected {
        SidebarRowSurface::Selected
    } else if has_unsent_draft {
        SidebarRowSurface::Draft
    } else if recede {
        SidebarRowSurface::Receded
    } else {
        SidebarRowSurface::Plain
    };
    let project_name = context.project_names.get(&thread.project).cloned();
    SidebarThreadRow {
        id: thread.id.clone(),
        title: thread.title.clone(),
        section,
        variant,
        project_id: thread.project.clone(),
        project_name: project_name.clone(),
        branch: thread.branch.clone(),
        worktree_path: thread
            .worktree_path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(String::from),
        linked_pull_request: thread
            .linked_pull_request
            .as_ref()
            .map(|pr| SidebarPullRequest {
                number: pr.number,
                repository: pr.repository.clone(),
                url: pr.url.clone(),
            }),
        provider_stack: thread.provider_stack(),
        pinned: thread.pinned_at.is_some(),
        active,
        selected,
        status,
        top_status,
        working_started_at,
        trailing,
        unread,
        woke_at,
        recede,
        faded: recede && status == SidebarThreadStatus::Working,
        surface,
        title_tone,
        title_regenerating: thread.title_regenerating,
        has_unsent_draft,
        draggable: section != SidebarSection::Working,
        can_snooze,
        actions,
        accessibility_label: sidebar_row_accessibility(
            &thread.title,
            top_status.map(SidebarTopStatus::label),
            project_name.as_deref(),
            active,
        )
        .label,
    }
}

fn shelf(
    section: SidebarSection,
    name: &str,
    count: usize,
    expanded: bool,
    bottom_anchor: bool,
) -> SidebarItem {
    SidebarItem::Shelf {
        header: SidebarShelfHeader {
            section,
            label: if expanded {
                name.into()
            } else {
                format!("{name} ({count})")
            },
            count: count as u32,
            expanded,
            bottom_anchor,
        },
    }
}

fn draft_rows(
    snapshot: &Snapshot,
    scope: Option<&str>,
    project_names: &BTreeMap<String, String>,
) -> (Vec<SidebarDraftRow>, usize) {
    let open = snapshot
        .selected_thread
        .is_none()
        .then(|| snapshot.new_thread_draft_key());
    let mut count = 0;
    let mut rows = vec![];
    for (key, draft) in snapshot.drafts.iter() {
        let Some(project) = key.strip_prefix("new:") else {
            continue;
        };
        if draft.is_empty() || scope.is_some_and(|scope| scope != project) {
            continue;
        }
        count += 1;
        if open.as_deref() == Some(key.as_str()) {
            continue;
        }
        let first_line = draft.text.trim().lines().next().unwrap_or_default();
        let attachments = draft.attachments.len();
        let preview = if first_line.is_empty() {
            format!(
                "{attachments} attachment{}",
                if attachments == 1 { "" } else { "s" }
            )
        } else {
            first_line.into()
        };
        let project_name = project_names.get(project).cloned();
        rows.push(SidebarDraftRow {
            draft_key: key.clone(),
            project_id: project.into(),
            accessibility_label: sidebar_row_accessibility(
                &preview,
                Some("Unsent draft"),
                project_name.as_deref(),
                false,
            )
            .label,
            project_name,
            preview,
        });
    }
    (rows, count)
}

/// The sidebar at `now_ms`. With the Working section on, call
/// [`observe_inbox_returns`] first so `returns` is current.
pub fn sidebar(
    snapshot: &Snapshot,
    now_ms: i64,
    options: &SidebarOptions,
    returns: &InboxReturns,
) -> SidebarView {
    let (threads, projects) = shell_threads(snapshot);
    let inputs: Vec<_> = projects
        .iter()
        .map(|project| SidebarProjectInput {
            id: project.id.clone(),
            title: project.name.clone(),
            created_at: None,
            updated_at: None,
        })
        .collect();
    let inputs = if options.project_sort_order == SidebarProjectSortOrder::Manual {
        order_items_by_preferred_ids(inputs, &options.project_order, |project| {
            vec![project.id.clone()]
        })
    } else {
        inputs
    };
    let groups = sort_sidebar_project_groups(inputs, &threads, options.project_sort_order);
    let project_names: BTreeMap<_, _> = groups
        .iter()
        .map(|project| (project.id.clone(), project.title.clone()))
        .collect();
    let scope = snapshot
        .selected_project
        .as_deref()
        .filter(|scope| project_names.contains_key(*scope));
    let mut project_scope = vec![SidebarProjectScopeItem {
        project_id: None,
        label: "All projects".into(),
        selected: scope.is_none(),
    }];
    project_scope.extend(groups.iter().map(|project| SidebarProjectScopeItem {
        project_id: Some(project.id.clone()),
        label: project.title.clone(),
        selected: scope == Some(project.id.as_str()),
    }));

    let visible = filter_sidebar_visible_threads(threads, scope);
    let shelves = shelve(visible, now_ms, options.working_section, returns);
    let route = snapshot.selected_thread.as_ref().map(|id| id.as_str());
    let context = RowContext {
        now_ms,
        route,
        selection: &options.selection,
        project_names: &project_names,
        snapshot,
    };

    let page = SETTLED_TAIL_INITIAL_COUNT
        .saturating_add(SETTLED_TAIL_PAGE_COUNT.saturating_mul(options.settled_pages))
        as usize;
    let mut visible_settled: Vec<_> = shelves.settled.iter().take(page).collect();
    if let Some(open) = shelves
        .settled
        .iter()
        .skip(page)
        .find(|thread| Some(thread.id.as_str()) == route)
    {
        visible_settled.push(open);
    }
    let hidden_settled = shelves.settled.len() - visible_settled.len();
    let rendered_settled: Vec<_> = visible_settled
        .into_iter()
        .filter(|thread| options.settled_expanded || Some(thread.id.as_str()) == route)
        .collect();

    let mut items = vec![];
    if shelves.total() > 0 {
        let rows = |threads: &[&ThreadSummary], section| {
            threads
                .iter()
                .map(|thread| SidebarItem::Thread {
                    row: thread_row(thread, section, &context),
                })
                .collect::<Vec<_>>()
        };
        items.push(SidebarItem::Boundary {
            marker: SidebarListMarker::PinnedHeader,
        });
        items.extend(rows(
            &shelves.pinned.iter().collect::<Vec<_>>(),
            SidebarSection::Pinned,
        ));
        items.push(SidebarItem::Boundary {
            marker: SidebarListMarker::PinnedDivider,
        });
        items.push(SidebarItem::Boundary {
            marker: SidebarListMarker::ActivePlaceholder,
        });
        items.extend(rows(
            &shelves.active.iter().collect::<Vec<_>>(),
            SidebarSection::Active,
        ));
        if !shelves.working.is_empty() {
            items.push(shelf(
                SidebarSection::Working,
                "Working",
                shelves.working.len(),
                options.working_expanded,
                true,
            ));
            items.extend(rows(
                &collapsed(&shelves.working, options.working_expanded, route),
                SidebarSection::Working,
            ));
        }
        if !shelves.snoozed.is_empty() {
            items.push(shelf(
                SidebarSection::Snoozed,
                "Snoozed",
                shelves.snoozed.len(),
                options.snoozed_expanded,
                shelves.working.is_empty(),
            ));
            items.extend(rows(
                &collapsed(&shelves.snoozed, options.snoozed_expanded, route),
                SidebarSection::Snoozed,
            ));
        }
        items.push(shelf(
            SidebarSection::Settled,
            "Settled",
            shelves.settled.len(),
            options.settled_expanded,
            shelves.working.is_empty() && shelves.snoozed.is_empty(),
        ));
        items.push(SidebarItem::Boundary {
            marker: SidebarListMarker::SettledPlaceholder,
        });
        items.extend(rows(&rendered_settled, SidebarSection::Settled));
        if options.settled_expanded && hidden_settled > 0 {
            items.push(SidebarItem::ShowMore {
                count: hidden_settled.min(SETTLED_TAIL_PAGE_COUNT as usize) as u32,
            });
        }
    }

    let (drafts, draft_count) = draft_rows(snapshot, scope, &project_names);
    let query = snapshot.search.trim();
    let search = (!query.is_empty()).then(|| {
        let matches: BTreeMap<_, _> = snapshot
            .search_matches
            .iter()
            .map(|found| (found.thread_id.to_string(), found))
            .collect();
        let content: BTreeSet<_> = matches.keys().cloned().collect();
        let searchable: Vec<_> = [
            &shelves.pinned,
            &shelves.active,
            &shelves.working,
            &shelves.snoozed,
            &shelves.settled,
        ]
        .into_iter()
        .flatten()
        .collect();
        let results: Vec<_> = crate::view::search::search_threads(searchable, query, &content)
            .into_iter()
            .map(|thread| {
                let found = matches.get(&thread.id);
                let project_name = project_names.get(&thread.project).cloned();
                let active = route == Some(thread.id.as_str());
                SidebarSearchResult {
                    id: thread.id.clone(),
                    title: thread.title.clone(),
                    project_id: thread.project.clone(),
                    time_label: thread_time_label(thread, now_ms),
                    active,
                    snippet: found.map(|found| found.snippet.clone()),
                    snippet_source: found.map(|found| match found.source {
                        SearchSource::User => SidebarMatchSource::User,
                        SearchSource::Assistant => SidebarMatchSource::Assistant,
                    }),
                    accessibility_label: sidebar_row_accessibility(
                        &thread.title,
                        None,
                        project_name.as_deref(),
                        active,
                    )
                    .label,
                    project_name,
                }
            })
            .collect();
        SidebarSearchView {
            query: snapshot.search.clone(),
            empty_label: results.is_empty().then(|| "No threads found".into()),
            results,
        }
    });
    let empty_state = (search.is_none() && draft_count == 0 && shelves.total() == 0).then(|| {
        if projects.is_empty() {
            SidebarEmptyState::NoProjects
        } else if let Some(scope) = scope {
            SidebarEmptyState::NoThreadsInProject {
                project_name: project_names[scope].clone(),
            }
        } else {
            SidebarEmptyState::NoThreads
        }
    });
    SidebarView {
        drafts,
        items,
        search,
        empty_state,
        project_scope,
        inbox_time_ordered: options.working_section,
    }
}

/// What dropping `active_key` on `over_id` does, against the rows `view`
/// rendered from `snapshot`.
pub fn plan_sidebar_drop(
    snapshot: &Snapshot,
    view: &SidebarView,
    active_key: &str,
    over_id: &str,
) -> SidebarThreadDropPlan {
    let items = view.list_items();
    let (Some(row), Some(target)) = (
        view.row(active_key),
        resolve_sidebar_drop_target(&items, active_key, over_id),
    ) else {
        return SidebarThreadDropPlan::None;
    };
    let (threads, _) = shell_threads(snapshot);
    let keys = |key: fn(&ThreadSummary) -> Option<String>| -> BTreeMap<String, Option<String>> {
        threads
            .iter()
            .map(|thread| (thread.id.clone(), key(thread)))
            .collect()
    };
    let in_section = |section| -> Vec<String> {
        view.rows()
            .filter(|row| row.section == section)
            .map(|row| row.id.clone())
            .collect()
    };
    let settled = threads
        .iter()
        .find(|thread| thread.id == active_key)
        .is_some_and(|thread| thread.settled_override == Some(SettledOverride::Settled));
    plan_sidebar_thread_drop(&SidebarDropInput {
        active_key,
        active_section: row.section,
        active_pinned: Some(row.pinned),
        active_settled: Some(settled),
        target: &target,
        pinned_order: &in_section(SidebarSection::Pinned),
        pinned_keys: &keys(|thread| thread.pin_order_key.clone()),
        active_order: &in_section(SidebarSection::Active),
        active_keys: &keys(|thread| thread.active_order_key.clone()),
        active_time_ordered: view.inbox_time_ordered,
    })
}

#[cfg(test)]
mod tests;
