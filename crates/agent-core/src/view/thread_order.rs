//! Moving list rows: where a move or drop lands, the key writes that realize
//! it, the order held on screen until the Host confirms those writes, and the
//! lifecycle changes a cross-section drop makes.
use super::snooze::effective_snoozed;
use super::thread_list::ordered_section;
use super::thread_sort::{OrderAssignment, plan_pinned_reorder};
use super::thread_summary::{SettledOverride, ThreadSummary};
use std::collections::{BTreeMap, BTreeSet};

/// The arranged sections a move can reorder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum OrderSection {
    Pinned,
    Active,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DropSection {
    Pinned,
    Active,
    Settled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum Placement {
    Before,
    After,
}

/// Move up/down by one displayed row, or a drop next to a target. A drop
/// with a section may come from another section; without a target it lands
/// at the section's start or end.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum MoveDestination {
    Up,
    Down,
    Drop {
        target: Option<String>,
        section: Option<DropSection>,
        placement: Placement,
    },
}

/// The order after a move, resolved against every row of the section
/// (including rows a filter hides); `None` when nothing would change.
pub fn thread_order_after_move(
    ordered: &[String],
    moved: &str,
    destination: &MoveDestination,
) -> Option<Vec<String>> {
    let from = ordered.iter().position(|id| id == moved);
    let mut result: Vec<String> = ordered.iter().filter(|id| *id != moved).cloned().collect();
    let to = match destination {
        MoveDestination::Drop {
            section: Some(DropSection::Settled),
            ..
        } => return None,
        MoveDestination::Up => from?.checked_sub(1)?,
        MoveDestination::Down => Some(from? + 1).filter(|to| *to < ordered.len())?,
        MoveDestination::Drop {
            target: None,
            section: None,
            ..
        } => return None,
        MoveDestination::Drop {
            target: None,
            placement,
            ..
        } => match placement {
            Placement::Before => 0,
            Placement::After => result.len(),
        },
        MoveDestination::Drop {
            target: Some(target),
            section,
            placement,
        } => {
            if from.is_none() && section.is_none() {
                return None;
            }
            let index = result.iter().position(|id| id == target)?;
            index + usize::from(*placement == Placement::After)
        }
    };
    if Some(to) == from {
        return None;
    }
    result.insert(to, moved.to_owned());
    Some(result)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RowOrder {
    key: Option<String>,
    anchor: Option<i64>,
}

fn row_order(row: &ThreadSummary, section: OrderSection) -> RowOrder {
    match section {
        OrderSection::Pinned => RowOrder {
            key: row.pin_order_key.clone(),
            anchor: row.pinned_at,
        },
        OrderSection::Active => RowOrder {
            key: row.active_order_key.clone(),
            anchor: Some(row.unsettled_at.unwrap_or(row.created_at)),
        },
    }
}

/// Plans key writes for moves within one section. Every visible row stays an
/// anchor; keys of rows outside `ordered` stay reserved.
#[derive(Debug, Clone)]
pub struct ThreadMovePlanner {
    ordered: Vec<String>,
    keys: BTreeMap<String, Option<String>>,
}

impl ThreadMovePlanner {
    /// `all` defaults to `ordered`; pass every thread to keep hidden keys
    /// reserved and to plan a drop from another section.
    pub fn new<T: AsRef<ThreadSummary>>(
        ordered: &[T],
        all: Option<&[ThreadSummary]>,
        section: OrderSection,
    ) -> Self {
        let keys = match all {
            Some(all) => all
                .iter()
                .map(|row| (row.id.clone(), row_order(row, section).key))
                .collect(),
            None => ordered
                .iter()
                .map(|row| {
                    (
                        row.as_ref().id.clone(),
                        row_order(row.as_ref(), section).key,
                    )
                })
                .collect(),
        };
        Self {
            ordered: ordered.iter().map(|row| row.as_ref().id.clone()).collect(),
            keys,
        }
    }

    pub fn plan(&self, moved: &str, destination: &MoveDestination) -> Option<Vec<OrderAssignment>> {
        if !self.keys.contains_key(moved) {
            return None;
        }
        let next = thread_order_after_move(&self.ordered, moved, destination)?;
        let assignments = plan_pinned_reorder(&next, &self.keys, moved);
        (!assignments.is_empty()
            && assignments
                .iter()
                .all(|assignment| self.keys.contains_key(&assignment.id)))
        .then_some(assignments)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MoveAvailability {
    pub can_move_up: bool,
    pub can_move_down: bool,
}

/// Move up/down availability for every row of a section, answered by the same
/// planner that executes the move. A reorder in flight locks the whole list
/// until it is confirmed.
pub fn move_availability<T: AsRef<ThreadSummary>>(
    ordered: &[T],
    all: Option<&[ThreadSummary]>,
    section: OrderSection,
    pending: Option<&PendingThreadOrder>,
) -> BTreeMap<String, MoveAvailability> {
    if pending.is_some() {
        return BTreeMap::new();
    }
    let planner = ThreadMovePlanner::new(ordered, all, section);
    ordered
        .iter()
        .map(|row| {
            let id = &row.as_ref().id;
            let availability = MoveAvailability {
                can_move_up: planner.plan(id, &MoveDestination::Up).is_some(),
                can_move_down: planner.plan(id, &MoveDestination::Down).is_some(),
            };
            (id.clone(), availability)
        })
        .collect()
}

/// A move whose key writes the Host has not all confirmed. The list shows
/// `ordered_ids` until every write lands; membership or other arrangement
/// changes release it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingThreadOrder {
    pub section: OrderSection,
    pub ordered_ids: Vec<String>,
    before: BTreeMap<String, RowOrder>,
    assignments: BTreeMap<String, String>,
    confirmed: BTreeSet<String>,
    /// Every write command got its receipt.
    pub commands_complete: bool,
}

impl PendingThreadOrder {
    /// `None` when the move does not change the order.
    pub fn begin<T: AsRef<ThreadSummary>>(
        section: OrderSection,
        ordered: &[T],
        moved: &str,
        destination: &MoveDestination,
        assignments: &[OrderAssignment],
    ) -> Option<Self> {
        let ids: Vec<String> = ordered.iter().map(|row| row.as_ref().id.clone()).collect();
        Some(Self {
            section,
            ordered_ids: thread_order_after_move(&ids, moved, destination)?,
            before: ordered
                .iter()
                .map(|row| {
                    let row = row.as_ref();
                    (row.id.clone(), row_order(row, section))
                })
                .collect(),
            assignments: assignments
                .iter()
                .map(|assignment| (assignment.id.clone(), assignment.order_key.clone()))
                .collect(),
            confirmed: BTreeSet::new(),
            commands_complete: false,
        })
    }

    /// Receipts and shell updates arrive independently. Only this move's own
    /// key writes may pass through the hold; `None` releases it.
    pub fn reconcile<T: AsRef<ThreadSummary>>(&self, ordered: &[T]) -> Option<Self> {
        if ordered.len() != self.before.len() {
            return None;
        }
        let mut confirmed = self.confirmed.clone();
        for row in ordered {
            let row = row.as_ref();
            let before = self.before.get(&row.id)?;
            let current = row_order(row, self.section);
            if current.anchor != before.anchor {
                return None;
            }
            match self.assignments.get(&row.id) {
                Some(assigned) if current.key.as_ref() == Some(assigned) => {
                    confirmed.insert(row.id.clone());
                }
                _ if current.key != before.key || confirmed.contains(&row.id) => return None,
                _ => {}
            }
        }
        if self.commands_complete && confirmed.len() == self.assignments.len() {
            return None;
        }
        Some(Self {
            confirmed,
            ..self.clone()
        })
    }

    /// Reconciles against the section as the list currently shows it.
    pub fn refresh(
        &self,
        threads: &[ThreadSummary],
        now_ms: i64,
        queued: &BTreeSet<String>,
    ) -> Option<Self> {
        self.reconcile(&ordered_section(
            threads,
            self.section,
            None,
            now_ms,
            queued,
        ))
    }

    /// Marks every write as receipted, then refreshes.
    pub fn complete(
        &self,
        threads: &[ThreadSummary],
        now_ms: i64,
        queued: &BTreeSet<String>,
    ) -> Option<Self> {
        Self {
            commands_complete: true,
            ..self.clone()
        }
        .refresh(threads, now_ms, queued)
    }
}

/// Applies the full section's pending order after search or project filtering.
pub fn apply_pending_thread_order<T: AsRef<ThreadSummary>>(
    mut rows: Vec<T>,
    section: OrderSection,
    pending: Option<&PendingThreadOrder>,
) -> Vec<T> {
    let Some(pending) = pending.filter(|pending| pending.section == section) else {
        return rows;
    };
    let rank: BTreeMap<&str, usize> = pending
        .ordered_ids
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect();
    rows.sort_by_key(|row| {
        rank.get(row.as_ref().id.as_str())
            .copied()
            .unwrap_or(usize::MAX)
    });
    rows
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DropLifecycle {
    pub pin: bool,
    pub unpin: bool,
    pub unsettle: bool,
    pub unsnooze: bool,
}

/// A drop on Pinned pins (the Host wakes the thread); a drop on Active clears
/// each parked state before the order key is written.
pub fn thread_drop_lifecycle(
    thread: &ThreadSummary,
    section: OrderSection,
    now_ms: i64,
) -> DropLifecycle {
    match section {
        OrderSection::Pinned => DropLifecycle {
            pin: true,
            ..DropLifecycle::default()
        },
        OrderSection::Active => DropLifecycle {
            pin: false,
            unpin: thread.pinned_at.is_some(),
            unsettle: thread.settled_override == Some(SettledOverride::Settled),
            unsnooze: effective_snoozed(thread, now_ms),
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DragSection {
    Pinned,
    Active,
    Snoozed,
    Settled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DragAction {
    Settle,
    Reorder,
    Pin,
    Unpin,
    Unsettle,
    Unsnooze,
}
impl DragAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Settle => "Settle",
            Self::Reorder => "Reorder",
            Self::Pin => "Pin",
            Self::Unpin => "Unpin",
            Self::Unsettle => "Unsettle",
            Self::Unsnooze => "Unsnooze",
        }
    }
}

/// The lifecycle change a drop makes, shown while hovering; `None` where a
/// drop does nothing (snooze needs a wake time, settled has no order).
pub fn thread_drag_action(source: DragSection, destination: DragSection) -> Option<DragAction> {
    Some(match (source, destination) {
        (_, DragSection::Snoozed) | (DragSection::Settled, DragSection::Settled) => return None,
        (_, DragSection::Settled) => DragAction::Settle,
        (source, destination) if source == destination => DragAction::Reorder,
        (_, DragSection::Pinned) => DragAction::Pin,
        (DragSection::Pinned, _) => DragAction::Unpin,
        (DragSection::Settled, _) => DragAction::Unsettle,
        _ => DragAction::Unsnooze,
    })
}

/// How far a row shifts while a lifted row hovers at `insertion_offset`; hit
/// testing keeps the original layout.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn drag_gap_offset(
    row_offset: f64,
    source_offset: f64,
    source_height: f64,
    insertion_offset: f64,
) -> f64 {
    if row_offset == source_offset {
        return 0.0;
    }
    if insertion_offset <= source_offset {
        return if row_offset >= insertion_offset && row_offset < source_offset {
            source_height
        } else {
            0.0
        };
    }
    if row_offset > source_offset && row_offset < insertion_offset {
        -source_height
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests;
