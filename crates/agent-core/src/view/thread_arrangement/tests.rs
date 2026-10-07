use super::*;
use crate::sync::ShellCache;
use crate::view::thread_summary::fixtures::ms;
use agent_domain::{ThreadId, ThreadShell, Timestamp};
use agent_protocol::conversation::ShellSnapshot;
use std::sync::Arc;

const NOW: &str = "2026-06-02T00:00:00.000Z";

fn at(iso: &str) -> Timestamp {
    Timestamp::parse(iso).unwrap()
}

fn shell_row(id: &str, created: &str) -> ThreadShell {
    let mut row = agent_domain::shell(&crate::sync::fixtures::thread_state(id)).unwrap();
    row.id = ThreadId::new(id).unwrap();
    row.created_at = at(created);
    row
}

fn snapshot() -> Snapshot {
    let mut pinned = shell_row("pinned", "2026-05-01T00:00:00.000Z");
    pinned.pinned_at = Some(at("2026-05-02T00:00:00.000Z"));
    let mut snoozed = shell_row("snoozed", "2026-05-01T00:00:00.000Z");
    snoozed.snoozed_at = Some(at("2026-06-01T23:00:00.000Z"));
    snoozed.snoozed_until = Some(at("2026-06-02T05:00:00.000Z"));
    let mut settled = shell_row("settled", "2026-05-01T00:00:00.000Z");
    settled.settled = Some(true);
    settled.settled_at = Some(at("2026-05-20T00:00:00.000Z"));
    Snapshot {
        shell: Arc::new(ShellCache::from_cache(ShellSnapshot {
            snapshot_sequence: 1,
            projects: vec![],
            threads: vec![
                pinned,
                shell_row("older", "2026-05-03T00:00:00.000Z"),
                shell_row("newer", "2026-05-04T00:00:00.000Z"),
                snoozed,
                settled,
            ],
        })),
        ..Snapshot::default()
    }
}

const OPEN: ArrangementOptions = ArrangementOptions {
    snoozed_expanded: true,
    settled_expanded: true,
};

fn keys(view: &ThreadArrangementView) -> Vec<&str> {
    view.rows.iter().map(|row| row.key.as_str()).collect()
}

fn moves(view: &ThreadArrangementView, key: &str) -> Vec<(DropSection, String)> {
    let row = view.rows.iter().find(|row| row.key == key).unwrap();
    let ArrangementRowKind::Thread { section_moves, .. } = &row.kind else {
        panic!("thread row")
    };
    section_moves
        .iter()
        .map(|item| (item.section, item.label.clone()))
        .collect()
}

#[test]
fn lists_each_section_under_a_counted_header_and_folds_parked_threads() {
    let snapshot = snapshot();
    let folded = thread_arrangement(&snapshot, ms(NOW), ArrangementOptions::default());
    assert_eq!(
        keys(&folded),
        [
            "section:pinned",
            "pinned",
            "section:active",
            "newer",
            "older",
            "section:snoozed",
            "section:settled"
        ]
    );
    let labels: Vec<&str> = folded
        .rows
        .iter()
        .filter_map(|row| match &row.kind {
            ArrangementRowKind::Header { label, .. } => Some(label.as_str()),
            ArrangementRowKind::Thread { .. } => None,
        })
        .collect();
    assert_eq!(
        labels,
        ["Pinned (1)", "Active (2)", "Snoozed (1)", "Settled (1)"]
    );
    let open = thread_arrangement(&snapshot, ms(NOW), OPEN);
    assert_eq!(
        &keys(&open)[5..],
        ["section:snoozed", "snoozed", "section:settled", "settled"]
    );
    assert!(!open.locked);
}

#[test]
fn offers_the_section_moves_a_drop_there_would_make() {
    let view = thread_arrangement(&snapshot(), ms(NOW), OPEN);
    let label = |section, text: &str| (section, text.to_owned());
    assert_eq!(
        moves(&view, "pinned"),
        [
            label(DropSection::Active, "Unpin"),
            label(DropSection::Settled, "Settle")
        ]
    );
    assert_eq!(
        moves(&view, "newer"),
        [
            label(DropSection::Pinned, "Pin"),
            label(DropSection::Settled, "Settle")
        ]
    );
    assert_eq!(
        moves(&view, "snoozed"),
        [
            label(DropSection::Pinned, "Pin"),
            label(DropSection::Active, "Unsnooze"),
            label(DropSection::Settled, "Settle")
        ]
    );
    assert_eq!(
        moves(&view, "settled"),
        [
            label(DropSection::Pinned, "Pin"),
            label(DropSection::Active, "Unsettle")
        ]
    );
}

#[test]
fn a_dropped_row_lands_after_the_row_above_or_first_under_a_header() {
    let snapshot = snapshot();
    let landing =
        |moved: &str, to: u32| thread_arrangement_move(&snapshot, ms(NOW), OPEN, moved, to);
    let pin = landing("older", 2).unwrap();
    assert_eq!(pin.section, OrderSection::Pinned);
    assert_eq!(
        pin.destination,
        MoveDestination::Drop {
            target: Some("pinned".into()),
            section: Some(DropSection::Pinned),
            placement: Placement::After,
        }
    );
    assert_eq!(pin.label, "Pin");
    let first = landing("older", 3).unwrap();
    assert_eq!(
        first.destination,
        MoveDestination::Drop {
            target: None,
            section: Some(DropSection::Active),
            placement: Placement::Before,
        }
    );
    assert_eq!(first.label, "Reorder");
    assert_eq!(landing("newer", 9).unwrap().label, "Settle");
    assert_eq!(landing("older", 7), None, "snoozing needs a wake time");
    assert_eq!(landing("older", 5), None, "dropped where it was");
    assert_eq!(
        landing("older", 0),
        None,
        "nothing is above the first header"
    );
}

#[test]
fn a_hovering_thread_can_pin_or_settle_and_cannot_drop_onto_itself() {
    let snapshot = snapshot();
    let pin = thread_arrangement_drop(&snapshot, ms(NOW), OPEN, "older", "pinned", true).unwrap();
    assert_eq!(pin.label, "Pin");
    assert_eq!(
        pin.destination,
        MoveDestination::Drop {
            target: Some("pinned".into()),
            section: Some(DropSection::Pinned),
            placement: Placement::After,
        }
    );
    assert_eq!(
        thread_arrangement_drop(&snapshot, ms(NOW), OPEN, "older", "section:settled", false)
            .unwrap()
            .label,
        "Settle"
    );
    assert_eq!(
        thread_arrangement_drop(&snapshot, ms(NOW), OPEN, "older", "older", false),
        None
    );
}
