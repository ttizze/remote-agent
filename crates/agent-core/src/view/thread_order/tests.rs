use super::*;
use crate::view::thread_sort::{
    generate_spread_pin_order_keys, pin_order_key_between, sort_pinned_threads_by_order_key,
};
use crate::view::thread_summary::fixtures::{ms, summary};
use proptest::prelude::*;

const NOW: &str = "2026-06-02T00:00:00.000Z";

fn now() -> i64 {
    ms(NOW)
}
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}
fn ids<T: AsRef<ThreadSummary>>(rows: &[T]) -> Vec<String> {
    rows.iter().map(|row| row.as_ref().id.clone()).collect()
}
fn thread(id: &str) -> ThreadSummary {
    ThreadSummary {
        project: "project-test".into(),
        created_at: ms("2026-01-01T00:00:00.000Z"),
        updated_at: ms("2026-01-01T00:00:00.000Z"),
        ..summary(id)
    }
}
fn keyed(id: &str, section: OrderSection, key: Option<&str>) -> ThreadSummary {
    let mut row = thread(id);
    match section {
        OrderSection::Pinned => {
            row.pinned_at = Some(now());
            row.pin_order_key = key.map(String::from);
        }
        OrderSection::Active => row.active_order_key = key.map(String::from),
    }
    row
}
fn rows(section: OrderSection, keys: &[Option<&str>]) -> Vec<ThreadSummary> {
    keys.iter()
        .enumerate()
        .map(|(index, key)| keyed(&format!("move-{index}"), section, *key))
        .collect()
}
fn drop_at(
    target: Option<&str>,
    section: Option<DropSection>,
    placement: Placement,
) -> MoveDestination {
    MoveDestination::Drop {
        target: target.map(String::from),
        section,
        placement,
    }
}
const SECTIONS: [OrderSection; 2] = [OrderSection::Active, OrderSection::Pinned];

#[test]
fn keeps_keyed_neighbors_as_usable_anchors() {
    for section in SECTIONS {
        let ordered = rows(section, &[Some("bb"), Some("dd"), Some("ff")]);
        let planner = ThreadMovePlanner::new(&ordered, None, section);
        let assignments = planner.plan("move-0", &MoveDestination::Down).unwrap();
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].id, "move-0");
        assert!(assignments[0].order_key.as_str() > "dd");
        assert!(assignments[0].order_key.as_str() < "ff");
        assert_eq!(planner.plan("move-0", &MoveDestination::Up), None);
    }
}

#[test]
fn materializes_keyless_rows_for_a_move() {
    for section in SECTIONS {
        let ordered = rows(section, &[None, None, None]);
        let planner = ThreadMovePlanner::new(&ordered, None, section);
        assert_eq!(
            planner
                .plan("move-0", &MoveDestination::Down)
                .unwrap()
                .len(),
            3
        );
    }
}

#[test]
fn reserves_snoozed_keys_when_moving_visible_rows() {
    for section in SECTIONS {
        let ordered = rows(section, &[Some("bb"), Some("dd"), Some("ff")]);
        let collision = ThreadMovePlanner::new(&ordered, None, section)
            .plan("move-0", &MoveDestination::Down)
            .unwrap()[0]
            .order_key
            .clone();
        let mut hidden = keyed("snoozed", section, Some(&collision));
        hidden.snoozed_at = Some(now());
        hidden.snoozed_until = Some(ms("2099-01-01T00:00:00.000Z"));
        let mut all = ordered.clone();
        all.push(hidden);
        let assignments = ThreadMovePlanner::new(&ordered, Some(&all), section)
            .plan("move-0", &MoveDestination::Down)
            .unwrap();
        assert_eq!(assignments.len(), 1);
        assert_ne!(assignments[0].order_key, collision);
        assert!(
            assignments[0].order_key.as_str() > "dd" && assignments[0].order_key.as_str() < "ff"
        );
    }
}

#[test]
fn moves_a_keyed_row_with_one_write_despite_keyless_rows_elsewhere() {
    let ordered = rows(
        OrderSection::Active,
        &[None, None, Some("bb"), Some("dd"), Some("ff")],
    );
    let assignments = ThreadMovePlanner::new(&ordered, None, OrderSection::Active)
        .plan("move-4", &MoveDestination::Up)
        .unwrap();
    assert_eq!(assignments.len(), 1);
    assert_eq!(assignments[0].id, "move-4");
    assert!(assignments[0].order_key.as_str() > "bb");
    assert!(assignments[0].order_key.as_str() < "dd");
}

#[test]
fn moves_across_multiple_rows_while_keeping_hidden_anchors_in_place() {
    let order = strings(&["a", "hidden", "b", "c"]);
    assert_eq!(
        thread_order_after_move(&order, "c", &drop_at(Some("a"), None, Placement::Before)),
        Some(strings(&["c", "a", "hidden", "b"]))
    );
    assert_eq!(
        thread_order_after_move(&order, "a", &drop_at(Some("b"), None, Placement::After)),
        Some(strings(&["hidden", "b", "a", "c"]))
    );
}

#[test]
fn rejects_missing_self_and_unchanged_destinations() {
    let order = strings(&["a", "b", "c"]);
    for target in ["missing", "a", "b"] {
        assert_eq!(
            thread_order_after_move(&order, "a", &drop_at(Some(target), None, Placement::Before)),
            None
        );
    }
    assert_eq!(
        thread_order_after_move(&strings(&["a", "b"]), "missing", &MoveDestination::Down),
        None
    );
}

#[test]
fn persists_a_dropped_row_and_holds_its_order_until_confirmed() {
    for section in SECTIONS {
        let ordered: Vec<ThreadSummary> = ["a", "b", "c", "d"]
            .iter()
            .map(|id| keyed(id, section, None))
            .collect();
        let destination = drop_at(Some("a"), None, Placement::Before);
        let assignments = ThreadMovePlanner::new(&ordered, None, section)
            .plan("d", &destination)
            .unwrap();
        let pending =
            PendingThreadOrder::begin(section, &ordered, "d", &destination, &assignments).unwrap();
        assert_eq!(pending.ordered_ids, strings(&["d", "a", "b", "c"]));
        let confirmed: Vec<ThreadSummary> = ordered
            .iter()
            .map(|row| {
                let key = assignments
                    .iter()
                    .find(|assignment| assignment.id == row.id)
                    .unwrap()
                    .order_key
                    .clone();
                keyed(&row.id, section, Some(&key))
            })
            .collect();
        assert_eq!(
            ids(&ordered_section(
                &confirmed,
                section,
                None,
                now(),
                &BTreeSet::new()
            )),
            pending.ordered_ids
        );
        let mut complete = pending.clone();
        complete.commands_complete = true;
        assert_eq!(complete.reconcile(&confirmed), None);
    }
}

#[test]
fn allows_a_long_drop_with_one_write() {
    let ordered = vec![
        thread("a"),
        thread("b"),
        keyed("c", OrderSection::Active, Some("h")),
        keyed("d", OrderSection::Active, Some("p")),
    ];
    let assignments = ThreadMovePlanner::new(&ordered, None, OrderSection::Active)
        .plan("a", &drop_at(Some("d"), None, Placement::After))
        .unwrap();
    assert_eq!(assignments.len(), 1);
    assert_eq!(assignments[0].id, "a");
    assert!(assignments[0].order_key.as_str() > "p");
}

#[test]
fn inserts_into_an_empty_section() {
    for (section, drop_section) in [
        (OrderSection::Pinned, DropSection::Pinned),
        (OrderSection::Active, DropSection::Active),
    ] {
        let source = thread("source");
        let destination = drop_at(None, Some(drop_section), Placement::Before);
        assert_eq!(
            thread_order_after_move(&[], "source", &destination),
            Some(strings(&["source"]))
        );
        let none: [ThreadSummary; 0] = [];
        let plan = ThreadMovePlanner::new(&none, Some(&[source]), section)
            .plan("source", &destination)
            .unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].id, "source");
    }
}

#[test]
fn places_an_incoming_row_between_existing_anchors_without_rewriting_them() {
    let a = ThreadSummary {
        pin_order_key: Some("h".into()),
        ..thread("a")
    };
    let b = ThreadSummary {
        pin_order_key: Some("z".into()),
        ..thread("b")
    };
    let source = thread("source");
    let destination = drop_at(Some("b"), Some(DropSection::Pinned), Placement::Before);
    let all = [a.clone(), b.clone(), source];
    let plan = ThreadMovePlanner::new(&[a, b], Some(&all), OrderSection::Pinned)
        .plan("source", &destination)
        .unwrap();
    assert_eq!(plan.len(), 1);
    assert!(plan[0].order_key.as_str() > "h" && plan[0].order_key.as_str() < "z");
    assert_eq!(
        thread_order_after_move(&strings(&["a", "b"]), "source", &destination),
        Some(strings(&["a", "source", "b"]))
    );
}

#[test]
fn rejects_removed_targets() {
    assert_eq!(
        thread_order_after_move(
            &strings(&["a"]),
            "source",
            &drop_at(Some("gone"), Some(DropSection::Active), Placement::Before)
        ),
        None
    );
}

#[test]
fn clears_pinning_settlement_and_snooze_when_returning_a_parked_thread_to_active() {
    let thread = ThreadSummary {
        pinned_at: Some(now()),
        settled_override: Some(SettledOverride::Settled),
        snoozed_at: Some(now()),
        snoozed_until: Some(ms("2099-01-01T00:00:00.000Z")),
        ..thread("parked")
    };
    assert_eq!(
        thread_drop_lifecycle(&thread, OrderSection::Active, now()),
        DropLifecycle {
            pin: false,
            unpin: true,
            unsettle: true,
            unsnooze: true,
        }
    );
    assert_eq!(
        thread_drop_lifecycle(&thread, OrderSection::Pinned, now()),
        DropLifecycle {
            pin: true,
            unpin: false,
            unsettle: false,
            unsnooze: false,
        }
    );
}

#[test]
fn does_not_send_lifecycle_commands_for_an_ordinary_active_reorder() {
    assert_eq!(
        thread_drop_lifecycle(&thread("active"), OrderSection::Active, now()),
        DropLifecycle::default()
    );
}

fn pinned_row(id: &str, key: Option<&str>) -> ThreadSummary {
    keyed(id, OrderSection::Pinned, key)
}
fn availability(can_move_up: bool, can_move_down: bool) -> MoveAvailability {
    MoveAvailability {
        can_move_up,
        can_move_down,
    }
}

#[test]
fn locks_every_row_while_a_pending_reorder_is_in_flight() {
    let rows = [pinned_row("t0", Some("a")), pinned_row("t1", Some("c"))];
    let pending =
        PendingThreadOrder::begin(OrderSection::Pinned, &rows, "t1", &MoveDestination::Up, &[])
            .unwrap();
    assert!(move_availability(&rows, None, OrderSection::Pinned, Some(&pending)).is_empty());
}

#[test]
fn rewrites_skip_rows_that_already_hold_their_key() {
    let rows = [
        pinned_row("t0", Some("f")),
        pinned_row("t1", Some("gn")),
        pinned_row("t2", None),
    ];
    let answers = move_availability(&rows, Some(&rows), OrderSection::Pinned, None);
    assert_eq!(answers["t0"], availability(false, true));
}

#[test]
fn keeps_moves_available_for_ids_containing_colons() {
    let rows = [
        pinned_row("thread:1", Some("a")),
        pinned_row("thread:2", Some("c")),
    ];
    let answers = move_availability(&rows, None, OrderSection::Pinned, None);
    assert_eq!(answers["thread:1"], availability(false, true));
    assert_eq!(answers["thread:2"], availability(true, false));
}

#[test]
fn denies_single_row_sections_in_both_directions() {
    let rows = [pinned_row("t0", Some("a"))];
    assert_eq!(
        move_availability(&rows, None, OrderSection::Pinned, None)["t0"],
        availability(false, false)
    );
}

#[test]
fn walks_past_a_hidden_reserved_key_at_every_adjacency_midpoint() {
    let visible = generate_spread_pin_order_keys(24);
    let ordered: Vec<ThreadSummary> = visible
        .iter()
        .enumerate()
        .map(|(index, key)| pinned_row(&format!("v{index}"), Some(key)))
        .collect();
    let mut all = ordered.clone();
    all.extend(
        visible
            .windows(2)
            .filter_map(|pair| pin_order_key_between(Some(&pair[0]), Some(&pair[1])))
            .enumerate()
            .map(|(index, key)| pinned_row(&format!("h{index}"), Some(&key))),
    );
    let answers = move_availability(&ordered, Some(&all), OrderSection::Pinned, None);
    for (index, row) in ordered.iter().enumerate() {
        assert_eq!(
            answers[&row.id],
            availability(index > 0, index + 1 < ordered.len())
        );
    }
}

#[test]
fn rewrites_a_section_of_consecutive_single_letter_keys() {
    let rows: Vec<ThreadSummary> = ["a", "b", "c", "d"]
        .iter()
        .enumerate()
        .map(|(index, key)| pinned_row(&format!("t{index}"), Some(key)))
        .collect();
    let answers = move_availability(&rows, Some(&rows), OrderSection::Pinned, None);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(answers[&row.id], availability(index > 0, index < 3));
    }
}

const KEY_POOL: [Option<&str>; 13] = [
    Some("a"),
    Some("b"),
    Some("ba"),
    Some("bb"),
    Some("c"),
    Some("ca"),
    Some("cb"),
    Some("d"),
    Some("h"),
    Some("n"),
    Some("t"),
    Some("z"),
    None,
];

proptest! {
    /// Every offered move exists and its writes produce exactly the swap.
    #[test]
    fn every_offered_move_realizes_the_adjacent_swap(
        visible in proptest::collection::vec(0..KEY_POOL.len(), 1..9),
        hidden in proptest::collection::vec(0..KEY_POOL.len(), 0..4),
    ) {
        let rows: Vec<ThreadSummary> = visible
            .iter()
            .enumerate()
            .map(|(index, key)| pinned_row(&format!("t:{index}"), KEY_POOL[*key]))
            .collect();
        let ordered = sort_pinned_threads_by_order_key(rows);
        let mut all = ordered.clone();
        all.extend(
            hidden
                .iter()
                .enumerate()
                .map(|(index, key)| pinned_row(&format!("h{index}"), KEY_POOL[*key])),
        );
        let answers = move_availability(&ordered, Some(&all), OrderSection::Pinned, None);
        let planner = ThreadMovePlanner::new(&ordered, Some(&all), OrderSection::Pinned);
        for (index, row) in ordered.iter().enumerate() {
            prop_assert_eq!(
                answers[&row.id],
                availability(index > 0, index + 1 < ordered.len())
            );
            for (direction, to) in [
                (MoveDestination::Up, index.checked_sub(1)),
                (MoveDestination::Down, Some(index + 1).filter(|to| *to < ordered.len())),
            ] {
                let Some(to) = to else { continue };
                let plan = planner.plan(&row.id, &direction).unwrap();
                let moved: Vec<ThreadSummary> = ordered
                    .iter()
                    .map(|row| {
                        let mut row = row.clone();
                        if let Some(write) = plan.iter().find(|write| write.id == row.id) {
                            row.pin_order_key = Some(write.order_key.clone());
                        }
                        row
                    })
                    .collect();
                let mut expected = ids(&ordered);
                expected.swap(index, to);
                prop_assert_eq!(ids(&sort_pinned_threads_by_order_key(moved)), expected);
            }
        }
    }
}

fn shared_fixture() -> (Vec<ThreadSummary>, PendingThreadOrder) {
    let rows: Vec<ThreadSummary> = [
        ("a", "2026-06-01T02:00:00.000Z"),
        ("b", "2026-06-01T01:00:00.000Z"),
    ]
    .iter()
    .map(|(id, created)| ThreadSummary {
        created_at: ms(created),
        ..thread(id)
    })
    .collect();
    let pending = PendingThreadOrder::begin(
        OrderSection::Active,
        &rows,
        "b",
        &MoveDestination::Up,
        &[
            OrderAssignment {
                id: "b".into(),
                order_key: "aa".into(),
            },
            OrderAssignment {
                id: "a".into(),
                order_key: "bb".into(),
            },
        ],
    )
    .unwrap();
    (rows, pending)
}
fn upsert(rows: &[ThreadSummary], id: &str, key: &str) -> Vec<ThreadSummary> {
    rows.iter()
        .map(|row| ThreadSummary {
            active_order_key: if row.id == id {
                Some(key.into())
            } else {
                row.active_order_key.clone()
            },
            ..row.clone()
        })
        .collect()
}

#[test]
fn blocks_another_pickup_after_receipts_and_clears_on_the_final_canonical_upsert() {
    let (rows, pending) = shared_fixture();
    let none = BTreeSet::new();
    let hold = pending.complete(&rows, now(), &none).unwrap();
    let rows = upsert(&rows, "b", "aa");
    let hold = hold.refresh(&rows, now(), &none).unwrap();
    let rows = upsert(&rows, "a", "bb");
    assert_eq!(hold.refresh(&rows, now(), &none), None);
}

#[test]
fn waits_for_receipts_when_shells_arrive_first() {
    let (rows, pending) = shared_fixture();
    let none = BTreeSet::new();
    let rows = upsert(&upsert(&rows, "b", "aa"), "a", "bb");
    let hold = pending.refresh(&rows, now(), &none).unwrap();
    assert_eq!(hold.complete(&rows, now(), &none), None);
}

#[test]
fn a_canonical_membership_change_invalidates_the_move() {
    let (rows, pending) = shared_fixture();
    assert_eq!(pending.refresh(&rows[1..], now(), &BTreeSet::new()), None);
}

#[test]
fn names_the_drag_action_for_each_destination_instead_of_its_section() {
    use DragSection::*;
    let label =
        |source, destination| thread_drag_action(source, destination).map(DragAction::label);
    assert_eq!(label(Active, Pinned), Some("Pin"));
    assert_eq!(label(Pinned, Active), Some("Unpin"));
    assert_eq!(label(Settled, Active), Some("Unsettle"));
    assert_eq!(label(Snoozed, Active), Some("Unsnooze"));
    assert_eq!(label(Active, Settled), Some("Settle"));
    assert_eq!(label(Pinned, Settled), Some("Settle"));
    assert_eq!(label(Active, Active), Some("Reorder"));
}

#[test]
fn does_not_offer_a_parked_section_reorder_or_a_snooze_without_a_wake_time() {
    assert_eq!(
        thread_drag_action(DragSection::Settled, DragSection::Settled),
        None
    );
    assert_eq!(
        thread_drag_action(DragSection::Active, DragSection::Snoozed),
        None
    );
    assert_eq!(
        thread_order_after_move(
            &strings(&["a", "b"]),
            "a",
            &drop_at(None, Some(DropSection::Settled), Placement::Before)
        ),
        None
    );
}

fn shifts(source: f64, insertion: f64) -> Vec<f64> {
    [0.0, 48.0, 120.0, 168.0, 240.0]
        .iter()
        .map(|offset| drag_gap_offset(*offset, source, 72.0, insertion))
        .collect()
}

#[test]
fn moves_the_active_header_and_intervening_rows_up_when_unpinning() {
    assert_eq!(shifts(48.0, 312.0), [0.0, 0.0, -72.0, -72.0, -72.0]);
}

#[test]
fn opens_a_full_gap_below_the_pinned_header_when_pinning() {
    assert_eq!(shifts(240.0, 48.0), [0.0, 72.0, 72.0, 72.0, 0.0]);
}

#[test]
fn leaves_the_source_gap_in_place_for_cancellation_or_its_current_destination() {
    assert_eq!(shifts(168.0, 168.0), [0.0; 5]);
    assert_eq!(shifts(168.0, 240.0), [0.0; 5]);
}

#[test]
fn moves_only_crossed_rows_for_an_adjacent_reorder() {
    assert_eq!(shifts(168.0, 312.0), [0.0, 0.0, 0.0, 0.0, -72.0]);
    assert_eq!(shifts(240.0, 168.0), [0.0, 0.0, 0.0, 72.0, 0.0]);
}
