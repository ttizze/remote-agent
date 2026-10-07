//! The mobile "Arrange threads" sheet: the Pinned, Active, Snoozed and Settled
//! sections as rows, what each thread row offers, and where a dragged thread
//! lands.
use super::inbox::{InboxReturns, sort_inbox_threads_by_return};
use super::snooze::effective_snoozed;
use super::thread_list::ordered_section;
use super::thread_order::{
    DragSection, DropSection, MoveDestination, OrderSection, PendingThreadOrder, Placement,
    ThreadMovePlanner, thread_drag_action,
};
use super::thread_summary::ThreadSummary;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArrangementOptions {
    pub snoozed_expanded: bool,
    pub settled_expanded: bool,
}

/// A move to another section the row offers without dragging.
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
    /// "Pinned (2)"; tapping Snoozed or Settled shows or hides its threads.
    Header { title: String, toggles: bool },
    Thread {
        thread_id: String,
        title: String,
        /// The handle starts a drag; off while a move is landing.
        draggable: bool,
        can_move_up: bool,
        can_move_down: bool,
        section_moves: Vec<ArrangementSectionMove>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArrangementRow {
    /// The section name for a header, the thread id for a thread.
    pub key: String,
    pub section: DragSection,
    pub kind: ArrangementRowKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadArrangementView {
    pub rows: Vec<ArrangementRow>,
}

/// Where a drop lands and what it does, for `Intent::MoveThread`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArrangementDrop {
    /// The section of the moved thread before the drop.
    pub source: OrderSection,
    pub destination: MoveDestination,
    /// The hovering row's caption: "Pin", "Reorder", "Settle"….
    pub action: Option<String>,
}

/// The sections in sheet order; a header's key is its lowercased title.
const SECTIONS: [(DragSection, &str); 4] = [
    (DragSection::Pinned, "Pinned"),
    (DragSection::Active, "Active"),
    (DragSection::Snoozed, "Snoozed"),
    (DragSection::Settled, "Settled"),
];

/// The sheet's sections and the planners of the arranged ones.
pub struct Arrangement<'a> {
    pinned: Vec<&'a ThreadSummary>,
    active: Vec<&'a ThreadSummary>,
    snoozed: Vec<&'a ThreadSummary>,
    settled: Vec<&'a ThreadSummary>,
    pinned_planner: ThreadMovePlanner,
    /// `None` while the Working beta orders the inbox by time.
    active_planner: Option<ThreadMovePlanner>,
    locked: bool,
}

fn drop_section(section: DragSection) -> Option<DropSection> {
    match section {
        DragSection::Pinned => Some(DropSection::Pinned),
        DragSection::Active => Some(DropSection::Active),
        DragSection::Settled => Some(DropSection::Settled),
        DragSection::Snoozed => None,
    }
}

fn drag_section(section: DropSection) -> DragSection {
    match section {
        DropSection::Pinned => DragSection::Pinned,
        DropSection::Active => DragSection::Active,
        DropSection::Settled => DragSection::Settled,
    }
}

impl<'a> Arrangement<'a> {
    pub fn new(
        threads: &'a [ThreadSummary],
        now_ms: i64,
        pending: Option<&PendingThreadOrder>,
        queued: &BTreeSet<String>,
        working_shelf: Option<&InboxReturns>,
    ) -> Self {
        let pinned = ordered_section(threads, OrderSection::Pinned, pending, now_ms, queued);
        let mut active = ordered_section(threads, OrderSection::Active, pending, now_ms, queued);
        if let Some(returns) = working_shelf {
            active = sort_inbox_threads_by_return(active, returns);
        }
        let visible: BTreeSet<&str> = pinned
            .iter()
            .chain(&active)
            .map(|thread| thread.id.as_str())
            .collect();
        let (snoozed, settled) = threads
            .iter()
            .filter(|thread| {
                thread.archived_at.is_none()
                    && !thread.subagent
                    && !visible.contains(thread.id.as_str())
            })
            .partition(|thread| effective_snoozed(thread, now_ms));
        Self {
            pinned_planner: ThreadMovePlanner::new(&pinned, Some(threads), OrderSection::Pinned),
            active_planner: working_shelf
                .is_none()
                .then(|| ThreadMovePlanner::new(&active, Some(threads), OrderSection::Active)),
            pinned,
            active,
            snoozed,
            settled,
            locked: pending.is_some(),
        }
    }

    fn rows_of(&self, section: DragSection) -> &[&'a ThreadSummary] {
        match section {
            DragSection::Pinned => &self.pinned,
            DragSection::Active => &self.active,
            DragSection::Snoozed => &self.snoozed,
            DragSection::Settled => &self.settled,
        }
    }

    fn section_of(&self, thread_id: &str) -> Option<DragSection> {
        SECTIONS
            .into_iter()
            .map(|(section, _)| section)
            .find(|section| self.rows_of(*section).iter().any(|row| row.id == thread_id))
    }

    fn planner(&self, section: OrderSection) -> Option<&ThreadMovePlanner> {
        match section {
            OrderSection::Pinned => Some(&self.pinned_planner),
            OrderSection::Active => self.active_planner.as_ref(),
        }
    }

    /// Whether `destination` in `section` moves the thread at all.
    fn plans(&self, thread_id: &str, section: OrderSection, destination: &MoveDestination) -> bool {
        self.planner(section)
            .is_some_and(|planner| planner.plan(thread_id, destination).is_some())
    }

    fn section_moves(&self, thread_id: &str, from: DragSection) -> Vec<ArrangementSectionMove> {
        [DropSection::Pinned, DropSection::Active, DropSection::Settled]
            .into_iter()
            .filter(|section| drag_section(*section) != from)
            .filter_map(|section| {
                let action = thread_drag_action(from, drag_section(section))?;
                let offered = match section {
                    DropSection::Settled => true,
                    DropSection::Pinned => self.plans(
                        thread_id,
                        OrderSection::Pinned,
                        &start_of(Some(DropSection::Pinned)),
                    ),
                    DropSection::Active => self.plans(
                        thread_id,
                        OrderSection::Active,
                        &start_of(Some(DropSection::Active)),
                    ),
                };
                offered.then(|| ArrangementSectionMove {
                    section,
                    label: action.label().into(),
                })
            })
            .collect()
    }

    pub fn view(&self, options: ArrangementOptions) -> ThreadArrangementView {
        let mut rows = vec![];
        for (section, title) in SECTIONS {
            let threads = self.rows_of(section);
            if section == DragSection::Snoozed && threads.is_empty() {
                continue;
            }
            let collapsible = matches!(section, DragSection::Snoozed | DragSection::Settled);
            rows.push(ArrangementRow {
                key: title.to_lowercase(),
                section,
                kind: ArrangementRowKind::Header {
                    title: format!("{title} ({})", threads.len()),
                    toggles: collapsible,
                },
            });
            let expanded = match section {
                DragSection::Snoozed => options.snoozed_expanded,
                DragSection::Settled => options.settled_expanded,
                _ => true,
            };
            if !expanded {
                continue;
            }
            let order = match section {
                DragSection::Pinned => Some(OrderSection::Pinned),
                DragSection::Active => Some(OrderSection::Active),
                _ => None,
            };
            rows.extend(threads.iter().map(|thread| ArrangementRow {
                key: thread.id.clone(),
                section,
                kind: ArrangementRowKind::Thread {
                    thread_id: thread.id.clone(),
                    title: thread.title.clone(),
                    draggable: !self.locked,
                    can_move_up: !self.locked
                        && order.is_some_and(|order| {
                            self.plans(&thread.id, order, &MoveDestination::Up)
                        }),
                    can_move_down: !self.locked
                        && order.is_some_and(|order| {
                            self.plans(&thread.id, order, &MoveDestination::Down)
                        }),
                    section_moves: if self.locked {
                        vec![]
                    } else {
                        self.section_moves(&thread.id, section)
                    },
                },
            }));
        }
        ThreadArrangementView { rows }
    }

    /// Dropping `thread_id` on the row `target_key` (a thread, or a section's
    /// header), before or `after` it. Snoozed takes no drops; Settled takes
    /// threads from elsewhere at its start.
    pub fn drop(&self, thread_id: &str, target_key: &str, after: bool) -> Option<ArrangementDrop> {
        if self.locked {
            return None;
        }
        let from = self.section_of(thread_id)?;
        let (section, target) = match self.section_of(target_key) {
            Some(section) => (section, Some(target_key.to_owned())),
            None => (
                SECTIONS
                    .into_iter()
                    .find(|(_, title)| title.to_lowercase() == target_key)?
                    .0,
                None,
            ),
        };
        let section = drop_section(section)?;
        let destination = match section {
            DropSection::Settled if from == DragSection::Settled => return None,
            DropSection::Settled => start_of(Some(DropSection::Settled)),
            _ => MoveDestination::Drop {
                placement: if target.is_some() && after {
                    Placement::After
                } else {
                    Placement::Before
                },
                target,
                section: Some(section),
            },
        };
        let order = match section {
            DropSection::Pinned => OrderSection::Pinned,
            DropSection::Active | DropSection::Settled => OrderSection::Active,
        };
        if section != DropSection::Settled && !self.plans(thread_id, order, &destination) {
            return None;
        }
        Some(ArrangementDrop {
            source: if from == DragSection::Pinned {
                OrderSection::Pinned
            } else {
                OrderSection::Active
            },
            destination,
            action: thread_drag_action(from, drag_section(section)).map(|action| action.label().into()),
        })
    }
}

/// A drop at the start of `section`.
fn start_of(section: Option<DropSection>) -> MoveDestination {
    MoveDestination::Drop {
        target: None,
        section,
        placement: Placement::Before,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::thread_summary::fixtures::summary;

    fn pinned(id: &str, key: &str) -> ThreadSummary {
        ThreadSummary {
            pinned_at: Some(1),
            pin_order_key: Some(key.into()),
            ..summary(id)
        }
    }

    fn active(id: &str, key: &str) -> ThreadSummary {
        ThreadSummary {
            active_order_key: Some(key.into()),
            ..summary(id)
        }
    }

    fn settled(id: &str) -> ThreadSummary {
        ThreadSummary {
            settled_override: Some(crate::view::thread_summary::SettledOverride::Settled),
            ..summary(id)
        }
    }

    fn keys(view: &ThreadArrangementView) -> Vec<&str> {
        view.rows.iter().map(|row| row.key.as_str()).collect()
    }

    #[test]
    fn lists_headers_and_hides_folded_sections_and_an_empty_snoozed_one() {
        let threads = [
            pinned("p", "a0"),
            active("a1", "a0"),
            active("a2", "a1"),
            settled("s"),
        ];
        let arrangement = Arrangement::new(&threads, 0, None, &BTreeSet::new(), None);
        let folded = arrangement.view(ArrangementOptions::default());
        assert_eq!(keys(&folded), ["pinned", "p", "active", "a1", "a2", "settled"]);
        let ArrangementRowKind::Header { title, toggles } = &folded.rows[5].kind else {
            panic!("expected the settled header");
        };
        assert_eq!((title.as_str(), *toggles), ("Settled (1)", true));
        let open = arrangement.view(ArrangementOptions {
            settled_expanded: true,
            ..ArrangementOptions::default()
        });
        assert_eq!(keys(&open).last(), Some(&"s"));
    }

    #[test]
    fn thread_rows_offer_steps_and_moves_to_other_sections() {
        let threads = [pinned("p", "a0"), active("a1", "a0"), active("a2", "a1")];
        let arrangement = Arrangement::new(&threads, 0, None, &BTreeSet::new(), None);
        let view = arrangement.view(ArrangementOptions::default());
        let ArrangementRowKind::Thread {
            can_move_up,
            can_move_down,
            section_moves,
            ..
        } = &view.rows[3].kind
        else {
            panic!("expected the first active thread");
        };
        assert!(!can_move_up && *can_move_down);
        let labels: Vec<&str> = section_moves.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(labels, ["Pin", "Settle"]);
    }

    #[test]
    fn a_drop_names_its_lifecycle_change_and_snoozed_takes_none() {
        let threads = [pinned("p", "a0"), active("a1", "a0"), active("a2", "a1")];
        let arrangement = Arrangement::new(&threads, 0, None, &BTreeSet::new(), None);
        let reorder = arrangement.drop("a2", "a1", false).unwrap();
        assert_eq!(reorder.action.as_deref(), Some("Reorder"));
        assert_eq!(reorder.source, OrderSection::Active);
        let pin = arrangement.drop("a1", "p", true).unwrap();
        assert_eq!(pin.action.as_deref(), Some("Pin"));
        assert_eq!(
            pin.destination,
            MoveDestination::Drop {
                target: Some("p".into()),
                section: Some(DropSection::Pinned),
                placement: Placement::After,
            }
        );
        let settle = arrangement.drop("p", "settled", false).unwrap();
        assert_eq!(settle.action.as_deref(), Some("Settle"));
        assert!(arrangement.drop("a1", "snoozed", false).is_none());
        assert!(arrangement.drop("a1", "a1", false).is_none());
    }

    #[test]
    fn a_landing_move_locks_the_handles() {
        let threads = [active("a1", "a0"), active("a2", "a1")];
        let ordered = ordered_section(&threads, OrderSection::Active, None, 0, &BTreeSet::new());
        let planner = ThreadMovePlanner::new(&ordered, Some(&threads), OrderSection::Active);
        let assignments = planner.plan("a2", &MoveDestination::Up).unwrap();
        let pending = PendingThreadOrder::begin(
            OrderSection::Active,
            &ordered,
            "a2",
            &MoveDestination::Up,
            &assignments,
        );
        let arrangement =
            Arrangement::new(&threads, 0, pending.as_ref(), &BTreeSet::new(), None);
        let view = arrangement.view(ArrangementOptions::default());
        assert!(view.rows.iter().all(|row| !matches!(
            row.kind,
            ArrangementRowKind::Thread { draggable: true, .. }
        )));
        assert!(arrangement.drop("a1", "a2", true).is_none());
    }
}
