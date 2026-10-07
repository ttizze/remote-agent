//! The "Arrange threads" sheet: every listed thread under its Pinned, Active,
//! Snoozed or Settled header, the moves each row offers, and where a row
//! dragged to a list position lands.
use super::inbox::sort_inbox_threads_by_return;
use super::snooze::effective_snoozed;
use super::thread_list::{ordered_section, queued_threads};
use super::thread_order::{
    DragSection, DropSection, MoveDestination, OrderSection, PendingThreadOrder, Placement,
    ThreadMovePlanner, thread_drag_action,
};
use super::thread_sort::sort_settled_threads;
use super::thread_summary::ThreadSummary;
use crate::state::Snapshot;
use std::collections::BTreeSet;

/// Which parked sections show their threads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArrangementOptions {
    pub snoozed_expanded: bool,
    pub settled_expanded: bool,
}

/// A move a row offers without dragging: into another section.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArrangementSectionMove {
    pub section: DropSection,
    /// "Pin", "Unpin", "Settle", "Unsettle" or "Unsnooze".
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ArrangementRowKind {
    /// "Pinned (2)"; the Snoozed and Settled headers fold their threads.
    Header { label: String, foldable: bool },
    Thread {
        thread_id: String,
        title: String,
        can_move_up: bool,
        can_move_down: bool,
        section_moves: Vec<ArrangementSectionMove>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArrangementRow {
    pub key: String,
    pub section: DragSection,
    pub kind: ArrangementRowKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadArrangementView {
    pub rows: Vec<ArrangementRow>,
    /// A move is still being saved: rows wait before moving again.
    pub locked: bool,
}

/// Where a dropped row lands: the `Intent::MoveThread` to send.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArrangementDrop {
    pub section: OrderSection,
    pub destination: MoveDestination,
    /// What the drop does, as `thread_drag_action` names it.
    pub label: String,
}

struct Sections<'a> {
    pinned: Vec<&'a ThreadSummary>,
    active: Vec<&'a ThreadSummary>,
    snoozed: Vec<&'a ThreadSummary>,
    settled: Vec<&'a ThreadSummary>,
    /// A time-ordered inbox has no slots, so Active takes no drops.
    active_reorderable: bool,
}

fn sections<'a>(
    threads: &'a [ThreadSummary],
    snapshot: &Snapshot,
    pending: Option<&PendingThreadOrder>,
    now_ms: i64,
) -> Sections<'a> {
    let queued = queued_threads(snapshot);
    let pinned = ordered_section(threads, OrderSection::Pinned, pending, now_ms, &queued);
    let ordered_active = ordered_section(threads, OrderSection::Active, pending, now_ms, &queued);
    let working_inbox = snapshot.preferences.working_section;
    let active = if working_inbox {
        sort_inbox_threads_by_return(ordered_active, &snapshot.inbox_returns)
    } else {
        ordered_active
    };
    let visible: BTreeSet<&str> = pinned
        .iter()
        .chain(&active)
        .map(|thread| thread.id.as_str())
        .collect();
    let (snoozed, settled): (Vec<_>, Vec<_>) = threads
        .iter()
        .filter(|thread| {
            thread.archived_at.is_none()
                && !thread.subagent
                && !visible.contains(thread.id.as_str())
        })
        .partition(|thread| effective_snoozed(thread, now_ms));
    let mut snoozed = snoozed;
    snoozed.sort_by_key(|thread| thread.snoozed_until.unwrap_or(0));
    Sections {
        pinned,
        active,
        snoozed,
        settled: sort_settled_threads(settled),
        active_reorderable: !working_inbox,
    }
}

impl Sections<'_> {
    fn threads(&self, section: DragSection) -> &[&ThreadSummary] {
        match section {
            DragSection::Pinned => &self.pinned,
            DragSection::Active => &self.active,
            DragSection::Snoozed => &self.snoozed,
            DragSection::Settled => &self.settled,
        }
    }

    fn planner(&self, all: &[ThreadSummary], section: OrderSection) -> Option<ThreadMovePlanner> {
        match section {
            OrderSection::Pinned => Some(ThreadMovePlanner::new(
                &self.pinned,
                Some(all),
                OrderSection::Pinned,
            )),
            OrderSection::Active => self
                .active_reorderable
                .then(|| ThreadMovePlanner::new(&self.active, Some(all), OrderSection::Active)),
        }
    }

    /// The landing of `moved` from `source` dropped next to `target` in
    /// `section`, if that drop does anything.
    fn drop(
        &self,
        all: &[ThreadSummary],
        moved: &str,
        source: DragSection,
        section: DragSection,
        target: Option<&str>,
        placement: Placement,
    ) -> Option<ArrangementDrop> {
        let label = thread_drag_action(source, section)?.label().to_owned();
        let (order, drop) = match section {
            DragSection::Pinned => (OrderSection::Pinned, DropSection::Pinned),
            DragSection::Active => (OrderSection::Active, DropSection::Active),
            DragSection::Settled => {
                return Some(ArrangementDrop {
                    section: OrderSection::Active,
                    destination: MoveDestination::Drop {
                        target: None,
                        section: Some(DropSection::Settled),
                        placement: Placement::Before,
                    },
                    label,
                });
            }
            DragSection::Snoozed => return None,
        };
        let destination = MoveDestination::Drop {
            target: target.map(str::to_owned),
            section: Some(drop),
            placement,
        };
        self.planner(all, order)?.plan(moved, &destination)?;
        Some(ArrangementDrop {
            section: order,
            destination,
            label,
        })
    }
}

const SECTIONS: [DragSection; 4] = [
    DragSection::Pinned,
    DragSection::Active,
    DragSection::Snoozed,
    DragSection::Settled,
];

fn section_name(section: DragSection) -> &'static str {
    match section {
        DragSection::Pinned => "Pinned",
        DragSection::Active => "Active",
        DragSection::Snoozed => "Snoozed",
        DragSection::Settled => "Settled",
    }
}

fn arrangement_rows(
    sections: &Sections<'_>,
    all: &[ThreadSummary],
    options: ArrangementOptions,
    locked: bool,
) -> Vec<ArrangementRow> {
    let mut rows = vec![];
    for section in SECTIONS {
        let threads = sections.threads(section);
        if section == DragSection::Snoozed && threads.is_empty() {
            continue;
        }
        let expanded = match section {
            DragSection::Snoozed => options.snoozed_expanded,
            DragSection::Settled => options.settled_expanded,
            _ => true,
        };
        rows.push(ArrangementRow {
            key: format!("section:{}", section_name(section).to_lowercase()),
            section,
            kind: ArrangementRowKind::Header {
                label: format!("{} ({})", section_name(section), threads.len()),
                foldable: matches!(section, DragSection::Snoozed | DragSection::Settled),
            },
        });
        if !expanded {
            continue;
        }
        let order = match section {
            DragSection::Pinned => Some(OrderSection::Pinned),
            DragSection::Active => Some(OrderSection::Active),
            _ => None,
        };
        let planner = order.and_then(|order| sections.planner(all, order));
        for thread in threads {
            let step = |destination| {
                !locked
                    && planner
                        .as_ref()
                        .is_some_and(|planner| planner.plan(&thread.id, &destination).is_some())
            };
            let section_moves = if locked {
                vec![]
            } else {
                [
                    DragSection::Pinned,
                    DragSection::Active,
                    DragSection::Settled,
                ]
                .into_iter()
                .filter(|target| *target != section)
                .filter_map(|target| {
                    let landing =
                        sections.drop(all, &thread.id, section, target, None, Placement::Before)?;
                    Some(ArrangementSectionMove {
                        section: match target {
                            DragSection::Pinned => DropSection::Pinned,
                            DragSection::Settled => DropSection::Settled,
                            _ => DropSection::Active,
                        },
                        label: landing.label,
                    })
                })
                .collect()
            };
            rows.push(ArrangementRow {
                key: thread.id.clone(),
                section,
                kind: ArrangementRowKind::Thread {
                    thread_id: thread.id.clone(),
                    title: thread.title.clone(),
                    can_move_up: step(MoveDestination::Up),
                    can_move_down: step(MoveDestination::Down),
                    section_moves,
                },
            });
        }
    }
    rows
}

fn summaries(snapshot: &Snapshot) -> Vec<ThreadSummary> {
    snapshot
        .shell_view()
        .map(|shell| {
            shell
                .threads
                .iter()
                .map(ThreadSummary::from_shell)
                .collect()
        })
        .unwrap_or_default()
}

fn pending(snapshot: &Snapshot) -> Option<&PendingThreadOrder> {
    snapshot.thread_order.as_ref().map(|hold| &hold.order)
}

/// The sheet's rows. While a move is saved, rows offer no moves.
pub fn thread_arrangement(
    snapshot: &Snapshot,
    now_ms: i64,
    options: ArrangementOptions,
) -> ThreadArrangementView {
    let threads = summaries(snapshot);
    let pending = pending(snapshot);
    let sections = sections(&threads, snapshot, pending, now_ms);
    let locked = pending.is_some();
    ThreadArrangementView {
        rows: arrangement_rows(&sections, &threads, options, locked),
        locked,
    }
}

/// Where `moved` lands when the sheet's list drops it before the row now at
/// `to_index` (the end when past the last row): after the row above, or at
/// the start of the section whose header is above.
pub fn thread_arrangement_move(
    snapshot: &Snapshot,
    now_ms: i64,
    options: ArrangementOptions,
    moved: &str,
    to_index: u32,
) -> Option<ArrangementDrop> {
    let threads = summaries(snapshot);
    let pending = pending(snapshot);
    if pending.is_some() {
        return None;
    }
    let sections = sections(&threads, snapshot, pending, now_ms);
    let rows = arrangement_rows(&sections, &threads, options, false);
    let from = rows.iter().position(|row| row.key == moved)?;
    let source = rows[from].section;
    let to = to_index as usize;
    if to == from || to == from + 1 {
        return None;
    }
    let above = &rows[to.checked_sub(1)?.min(rows.len() - 1)];
    let (target, placement) = match &above.kind {
        ArrangementRowKind::Thread { thread_id, .. } => {
            (Some(thread_id.as_str()), Placement::After)
        }
        ArrangementRowKind::Header { .. } => (None, Placement::Before),
    };
    sections.drop(&threads, moved, source, above.section, target, placement)
}

/// A drag hovering a visible row uses the same landing rule as the native list.
pub fn thread_arrangement_drop(
    snapshot: &Snapshot,
    now_ms: i64,
    options: ArrangementOptions,
    moved: &str,
    target: &str,
    after: bool,
) -> Option<ArrangementDrop> {
    if moved == target {
        return None;
    }
    let view = thread_arrangement(snapshot, now_ms, options);
    let index = view.rows.iter().position(|row| row.key == target)?;
    let destination = index
        + usize::from(after || matches!(view.rows[index].kind, ArrangementRowKind::Header { .. }));
    thread_arrangement_move(snapshot, now_ms, options, moved, destination as u32)
}

#[cfg(test)]
mod tests;
