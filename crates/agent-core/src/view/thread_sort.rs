//! Thread orders shared by the desktop sidebar and the mobile list: pinned
//! and active keys, settled recency and the legacy recency sort.
use super::thread_summary::ThreadSummary;
use crate::ordering;
use std::{cmp::Ordering, collections::BTreeMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ThreadSortOrder {
    #[default]
    UpdatedAt,
    CreatedAt,
}

impl AsRef<ThreadSummary> for ThreadSummary {
    fn as_ref(&self) -> &ThreadSummary {
        self
    }
}

/// The time a settled row sorts and labels by: the settle stamp, otherwise
/// the latest message or run stamp, then the update time.
pub fn settled_thread_timestamp(thread: &ThreadSummary) -> i64 {
    if let Some(settled) = thread.settled_at {
        return settled;
    }
    let run = thread.latest_run.as_ref();
    [
        thread.latest_user_message_at,
        run.and_then(|run| run.requested_at),
        run.and_then(|run| run.started_at),
        run.and_then(|run| run.completed_at),
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or(thread.updated_at)
}

/// Newest `key` first, then id.
pub(crate) fn sort_newest_first<T: AsRef<ThreadSummary>>(
    threads: Vec<T>,
    key: impl Fn(&T) -> i64,
) -> Vec<T> {
    let mut keyed: Vec<_> = threads
        .into_iter()
        .map(|thread| (key(&thread), thread))
        .collect();
    keyed.sort_by(|(left_ms, left), (right_ms, right)| {
        right_ms
            .cmp(left_ms)
            .then_with(|| left.as_ref().id.cmp(&right.as_ref().id))
    });
    keyed.into_iter().map(|(_, thread)| thread).collect()
}

/// Settled rows are history: newest work end first, then id.
pub fn sort_settled_threads<T: AsRef<ThreadSummary>>(threads: Vec<T>) -> Vec<T> {
    sort_newest_first(threads, |thread| settled_thread_timestamp(thread.as_ref()))
}

pub fn thread_sort_timestamp(thread: &ThreadSummary, order: ThreadSortOrder) -> i64 {
    match order {
        ThreadSortOrder::CreatedAt => thread.created_at,
        ThreadSortOrder::UpdatedAt => thread.latest_user_message_at.unwrap_or(thread.updated_at),
    }
}

/// Newest first; equal times put the greater id first.
pub fn sort_threads<T: AsRef<ThreadSummary>>(threads: Vec<T>, order: ThreadSortOrder) -> Vec<T> {
    let mut keyed: Vec<_> = threads
        .into_iter()
        .map(|thread| (thread_sort_timestamp(thread.as_ref(), order), thread))
        .collect();
    keyed.sort_by(|(left_ms, left), (right_ms, right)| {
        right_ms
            .cmp(left_ms)
            .then_with(|| right.as_ref().id.cmp(&left.as_ref().id))
    });
    keyed.into_iter().map(|(_, thread)| thread).collect()
}

pub fn latest_thread_for_project<'a>(
    threads: &'a [ThreadSummary],
    project: &str,
    order: ThreadSortOrder,
) -> Option<&'a ThreadSummary> {
    let mut latest: Option<(&ThreadSummary, i64)> = None;
    for thread in threads {
        if thread.project != project || thread.archived_at.is_some() {
            continue;
        }
        let timestamp = thread_sort_timestamp(thread, order);
        let newer = latest.is_none_or(|(current, current_ms)| {
            timestamp > current_ms || (timestamp == current_ms && thread.id > current.id)
        });
        if newer {
            latest = Some((thread, timestamp));
        }
    }
    latest.map(|(thread, _)| thread)
}

/// The active list anchors a thread at its creation, or at its return to the
/// active list when it was unsettled later.
pub fn active_thread_anchor_ms(thread: &ThreadSummary) -> i64 {
    thread.created_at.max(thread.unsettled_at.unwrap_or(0))
}

/// A key strictly between two neighbours; `None` for corrupt or unordered keys.
pub fn pin_order_key_between(before: Option<&str>, after: Option<&str>) -> Option<String> {
    ordering::between(before, after)
}

/// Evenly spaced keys for materializing an order.
pub fn generate_spread_pin_order_keys(count: usize) -> Vec<String> {
    ordering::spread(count)
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct OrderAssignment {
    pub id: String,
    pub order_key: String,
}

/// Writes that realize a new order: one key for the moved thread between
/// keyed neighbours, otherwise fresh keys for the whole section. Keys of
/// hidden rows in `keys` stay reserved.
pub fn plan_pinned_reorder(
    ordered: &[String],
    keys: &BTreeMap<String, Option<String>>,
    moved: &str,
) -> Vec<OrderAssignment> {
    ordering::reorder(ordered, keys, moved)
        .into_iter()
        .map(|(id, order_key)| OrderAssignment { id, order_key })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum MoveDirection {
    Up,
    Down,
}

/// Swaps the moved thread with its displayed neighbour; `None` off either end.
pub fn plan_pinned_move(
    ordered: &[String],
    keys: &BTreeMap<String, Option<String>>,
    moved: &str,
    direction: MoveDirection,
) -> Option<Vec<OrderAssignment>> {
    let from = ordered.iter().position(|id| id == moved)?;
    let to = match direction {
        MoveDirection::Up => from.checked_sub(1)?,
        MoveDirection::Down => Some(from + 1).filter(|to| *to < ordered.len())?,
    };
    let mut order = ordered.to_vec();
    let id = order.remove(from);
    order.insert(to, id);
    Some(plan_pinned_reorder(&order, keys, moved))
}

/// Arranged pins by key, then keyless pins newest created first.
pub fn sort_pinned_threads_by_order_key<T: AsRef<ThreadSummary>>(threads: Vec<T>) -> Vec<T> {
    let (mut keyed, mut keyless): (Vec<T>, Vec<T>) = threads
        .into_iter()
        .partition(|thread| thread.as_ref().pin_order_key.is_some());
    keyed.sort_by(|left, right| {
        let (left, right) = (left.as_ref(), right.as_ref());
        left.pin_order_key
            .cmp(&right.pin_order_key)
            .then_with(|| left.id.cmp(&right.id))
    });
    keyless.sort_by(|left, right| {
        let (left, right) = (left.as_ref(), right.as_ref());
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    keyed.extend(keyless);
    keyed
}

/// New and reopened threads lead the active list; arranged threads follow
/// their keys. Activity moves neither group.
pub fn sort_active_threads_by_order_key<T: AsRef<ThreadSummary>>(mut threads: Vec<T>) -> Vec<T> {
    threads.sort_by(|left, right| {
        let (left, right) = (left.as_ref(), right.as_ref());
        let order = match (&left.active_order_key, &right.active_order_key) {
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(left_key), Some(right_key)) => left_key.cmp(right_key),
            (None, None) => active_thread_anchor_ms(right).cmp(&active_thread_anchor_ms(left)),
        };
        order.then_with(|| left.id.cmp(&right.id))
    });
    threads
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::thread_summary::{
        RunSummary, RuntimeStatus,
        fixtures::{ms, summary},
    };

    fn ids<T: AsRef<ThreadSummary>>(threads: &[T]) -> Vec<String> {
        threads
            .iter()
            .map(|thread| thread.as_ref().id.clone())
            .collect()
    }
    fn keys(entries: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
        entries
            .iter()
            .map(|(id, key)| (id.to_string(), key.map(String::from)))
            .collect()
    }
    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }
    fn thread(id: &str, created_at: &str) -> ThreadSummary {
        ThreadSummary {
            created_at: ms(created_at),
            updated_at: ms(created_at),
            ..summary(id)
        }
    }

    #[test]
    fn uses_the_later_unsettle_time_when_an_old_thread_re_enters_the_active_list() {
        let thread = ThreadSummary {
            unsettled_at: Some(ms("2026-08-01T00:00:00.000Z")),
            ..thread("t", "2026-01-01T00:00:00.000Z")
        };
        assert_eq!(
            active_thread_anchor_ms(&thread),
            ms("2026-08-01T00:00:00.000Z")
        );
    }

    #[test]
    fn prefers_the_persisted_settlement_stamp_over_later_activity() {
        let thread = ThreadSummary {
            settled_at: Some(ms("2026-03-09T10:00:00.000Z")),
            latest_user_message_at: Some(ms("2026-03-09T11:00:00.000Z")),
            ..thread("t", "2026-03-09T12:00:00.000Z")
        };
        assert_eq!(
            settled_thread_timestamp(&thread),
            ms("2026-03-09T10:00:00.000Z")
        );
    }

    #[test]
    fn falls_back_to_the_latest_activity_when_the_stamp_is_missing() {
        let mut thread = ThreadSummary {
            latest_user_message_at: Some(ms("2026-03-09T11:00:00.000Z")),
            ..thread("t", "2026-03-09T12:00:00.000Z")
        };
        assert_eq!(
            settled_thread_timestamp(&thread),
            ms("2026-03-09T11:00:00.000Z")
        );
        thread.latest_user_message_at = None;
        assert_eq!(
            settled_thread_timestamp(&thread),
            ms("2026-03-09T12:00:00.000Z")
        );
    }

    fn settled(id: &str, settled_at: Option<&str>, message_at: Option<&str>) -> ThreadSummary {
        ThreadSummary {
            settled_at: settled_at.map(ms),
            latest_user_message_at: message_at.map(ms),
            ..thread(id, "2026-03-09T09:00:00.000Z")
        }
    }

    #[test]
    fn orders_by_settle_time_most_recently_settled_first() {
        let sorted = sort_settled_threads(vec![
            settled(
                "settled-first",
                Some("2026-03-09T10:00:00.000Z"),
                Some("2026-03-09T09:59:00.000Z"),
            ),
            settled(
                "settled-last",
                Some("2026-03-09T12:00:00.000Z"),
                Some("2026-03-09T08:00:00.000Z"),
            ),
        ]);
        assert_eq!(ids(&sorted), ["settled-last", "settled-first"]);
    }

    #[test]
    fn falls_back_to_last_activity_for_auto_settled_threads_without_a_settled_at_stamp() {
        let sorted = sort_settled_threads(vec![
            settled("auto-old", None, Some("2026-03-09T08:00:00.000Z")),
            settled("explicit", Some("2026-03-09T10:00:00.000Z"), None),
            settled("auto-recent", None, Some("2026-03-09T11:00:00.000Z")),
        ]);
        assert_eq!(ids(&sorted), ["auto-recent", "explicit", "auto-old"]);
    }

    #[test]
    fn counts_a_turn_completion_as_activity_for_auto_settled_threads() {
        let completed = ThreadSummary {
            latest_run: Some(RunSummary {
                requested_at: Some(ms("2026-03-09T10:00:00.000Z")),
                started_at: Some(ms("2026-03-09T10:00:00.000Z")),
                completed_at: Some(ms("2026-03-09T10:30:00.000Z")),
                ..crate::view::thread_summary::fixtures::run("run-1", RuntimeStatus::Completed)
            }),
            ..settled("completed-later", None, Some("2026-03-09T10:00:00.000Z"))
        };
        let sorted = sort_settled_threads(vec![
            settled("message-only", None, Some("2026-03-09T10:04:00.000Z")),
            completed,
        ]);
        assert_eq!(ids(&sorted), ["completed-later", "message-only"]);
    }

    #[test]
    fn breaks_timestamp_ties_by_id_so_the_order_is_stable() {
        let sorted = sort_settled_threads(vec![
            settled("b", Some("2026-03-09T10:00:00.000Z"), None),
            settled("a", Some("2026-03-09T10:00:00.000Z"), None),
        ]);
        assert_eq!(ids(&sorted), ["a", "b"]);
    }

    #[test]
    fn matches_the_per_comparison_order_on_a_shuffled_list_with_ties() {
        let stamps: [(Option<&str>, Option<&str>, &str); 4] = [
            (
                Some("2026-03-09T10:00:00.000Z"),
                None,
                "2026-03-09T09:00:00.000Z",
            ),
            (
                None,
                Some("2026-03-09T10:00:00.000Z"),
                "2026-03-09T09:00:00.000Z",
            ),
            (
                None,
                Some("2026-03-09T11:00:00.000Z"),
                "2026-03-09T09:00:00.000Z",
            ),
            (None, None, "2026-03-09T09:00:00.000Z"),
        ];
        let threads: Vec<_> = (0..30)
            .map(|index| {
                let row = (index * 7) % 30;
                let (settled_at, message_at, updated) = stamps[row % 4];
                (
                    row,
                    ThreadSummary {
                        settled_at: settled_at.map(ms),
                        latest_user_message_at: message_at.map(ms),
                        updated_at: ms(updated),
                        ..summary(&format!("thread-{}", row % 3))
                    },
                )
            })
            .collect();
        let mut expected = threads.clone();
        expected.sort_by(|(_, left), (_, right)| {
            settled_thread_timestamp(right)
                .cmp(&settled_thread_timestamp(left))
                .then_with(|| left.id.cmp(&right.id))
        });
        struct Row(usize, ThreadSummary);
        impl AsRef<ThreadSummary> for Row {
            fn as_ref(&self) -> &ThreadSummary {
                &self.1
            }
        }
        let sorted = sort_settled_threads(
            threads
                .into_iter()
                .map(|(row, thread)| Row(row, thread))
                .collect(),
        );
        assert_eq!(
            sorted.iter().map(|row| row.0).collect::<Vec<_>>(),
            expected.iter().map(|(row, _)| *row).collect::<Vec<_>>()
        );
    }

    #[test]
    fn keeps_input_order_and_descending_id_ties_for_both_sort_orders() {
        for order in [ThreadSortOrder::CreatedAt, ThreadSortOrder::UpdatedAt] {
            let threads = vec![
                thread("a", "2026-03-09T10:00:00.000Z"),
                thread("z", "2026-03-09T10:00:00.000Z"),
            ];
            assert_eq!(ids(&sort_threads(threads, order)), ["z", "a"]);
        }
    }

    #[test]
    fn sorts_threads_by_the_latest_user_message_in_recency_mode() {
        let sorted = sort_threads(
            vec![
                ThreadSummary {
                    updated_at: ms("2026-03-09T10:10:00.000Z"),
                    latest_user_message_at: Some(ms("2026-03-09T10:01:00.000Z")),
                    ..thread("thread-1", "2026-03-09T10:00:00.000Z")
                },
                ThreadSummary {
                    latest_user_message_at: Some(ms("2026-03-09T10:06:00.000Z")),
                    ..thread("thread-2", "2026-03-09T10:05:00.000Z")
                },
            ],
            ThreadSortOrder::UpdatedAt,
        );
        assert_eq!(ids(&sorted), ["thread-2", "thread-1"]);
    }

    #[test]
    fn falls_back_to_thread_timestamps_when_there_is_no_user_message() {
        let sorted = sort_threads(
            vec![
                ThreadSummary {
                    updated_at: ms("2026-03-09T10:01:00.000Z"),
                    ..thread("thread-1", "2026-03-09T10:00:00.000Z")
                },
                thread("thread-2", "2026-03-09T10:05:00.000Z"),
            ],
            ThreadSortOrder::UpdatedAt,
        );
        assert_eq!(ids(&sorted), ["thread-2", "thread-1"]);
    }

    #[test]
    fn can_sort_threads_by_created_at_when_configured() {
        let sorted = sort_threads(
            vec![
                thread("thread-1", "2026-03-09T10:05:00.000Z"),
                ThreadSummary {
                    updated_at: ms("2026-03-09T10:10:00.000Z"),
                    ..thread("thread-2", "2026-03-09T10:00:00.000Z")
                },
            ],
            ThreadSortOrder::CreatedAt,
        );
        assert_eq!(ids(&sorted), ["thread-1", "thread-2"]);
    }

    #[test]
    fn returns_the_latest_active_thread_for_a_project() {
        let threads = vec![
            ThreadSummary {
                updated_at: ms("2026-03-09T10:01:00.000Z"),
                ..thread("thread-1", "2026-03-09T10:00:00.000Z")
            },
            ThreadSummary {
                updated_at: ms("2026-03-09T10:10:00.000Z"),
                archived_at: Some(ms("2026-03-10T00:00:00.000Z")),
                ..thread("thread-2", "2026-03-09T10:05:00.000Z")
            },
            thread("thread-3", "2026-03-09T10:06:00.000Z"),
        ];
        let latest =
            latest_thread_for_project(&threads, "project-1", ThreadSortOrder::UpdatedAt).unwrap();
        assert_eq!(latest.id, "thread-3");
    }

    #[test]
    fn matches_the_first_sorted_eligible_thread_for_both_sort_orders() {
        for order in [ThreadSortOrder::CreatedAt, ThreadSortOrder::UpdatedAt] {
            let threads = vec![
                thread("a", "2026-03-09T10:00:00.000Z"),
                thread("z", "2026-03-09T10:00:00.000Z"),
                ThreadSummary {
                    archived_at: Some(ms("2026-03-10T00:00:00Z")),
                    ..thread("zz", "2026-03-09T10:00:00.000Z")
                },
                ThreadSummary {
                    project: "other".into(),
                    ..thread("zzz", "2026-03-09T10:00:00.000Z")
                },
            ];
            assert_eq!(
                latest_thread_for_project(&threads, "project-1", order).map(|t| &t.id),
                Some(&"z".to_string())
            );
            assert!(latest_thread_for_project(&[], "project-1", order).is_none());
            assert!(latest_thread_for_project(&threads, "missing", order).is_none());
            let twice = [threads[1].clone(), threads[1].clone()];
            assert!(std::ptr::eq(
                latest_thread_for_project(&twice, "project-1", order).unwrap(),
                &twice[0]
            ));
        }
    }

    #[test]
    fn keeps_hidden_slots_available_when_inserting_between_visible_neighbors() {
        let midpoint = pin_order_key_between(Some("f"), Some("t")).unwrap();
        let keys = keys(&[
            ("a", Some("f")),
            ("b", Some("t")),
            ("moved", Some("z")),
            ("snoozed", Some(&midpoint)),
        ]);
        let assignments = plan_pinned_reorder(&strings(&["a", "moved", "b"]), &keys, "moved");
        assert_eq!(assignments.len(), 1);
        let key = &assignments[0].order_key;
        assert!(key.as_str() > "f" && key.as_str() < "t");
        assert_ne!(key, &midpoint);
        assert_eq!(assignments[0].id, "moved");
    }

    #[test]
    fn materializes_keyless_rows_without_overwriting_hidden_slots() {
        let reserved = generate_spread_pin_order_keys(6);
        let mut entries = keys(&[("a", None), ("b", None), ("c", None)]);
        for (index, key) in reserved.iter().enumerate() {
            entries.insert(format!("hidden-{index}"), Some(key.clone()));
        }
        let assignments = plan_pinned_reorder(&strings(&["c", "a", "b"]), &entries, "c");
        assert_eq!(
            assignments
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>(),
            ["c", "a", "b"]
        );
        let written: Vec<_> = assignments.iter().map(|a| a.order_key.clone()).collect();
        let mut sorted = written.clone();
        sorted.sort();
        assert_eq!(written, sorted);
        sorted.dedup();
        assert_eq!(sorted.len(), 3);
        assert!(written.iter().all(|key| !reserved.contains(key)));
    }

    #[test]
    fn moves_a_thread_up_with_a_single_key_write() {
        let assignments = plan_pinned_move(
            &strings(&["a", "b", "c"]),
            &keys(&[("a", Some("f")), ("b", Some("m")), ("c", Some("t"))]),
            "c",
            MoveDirection::Up,
        )
        .unwrap();
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].id, "c");
        assert!(assignments[0].order_key.as_str() > "f" && assignments[0].order_key.as_str() < "m");
    }

    #[test]
    fn returns_none_when_the_move_falls_off_the_end_of_the_list() {
        let order = strings(&["a", "b"]);
        let keys = keys(&[("a", Some("f")), ("b", Some("m"))]);
        assert!(plan_pinned_move(&order, &keys, "a", MoveDirection::Up).is_none());
        assert!(plan_pinned_move(&order, &keys, "b", MoveDirection::Down).is_none());
    }

    #[test]
    fn materializes_keys_for_the_whole_section_when_a_neighbor_is_keyless() {
        let assignments = plan_pinned_move(
            &strings(&["a", "b", "c"]),
            &keys(&[("a", None), ("b", Some("m")), ("c", None)]),
            "b",
            MoveDirection::Up,
        )
        .unwrap();
        let written: Vec<_> = assignments.iter().map(|a| a.order_key.clone()).collect();
        let mut sorted = written.clone();
        sorted.sort();
        assert_eq!(sorted, written);
    }

    #[test]
    fn breaks_equal_pin_keys_by_id() {
        let sorted = sort_pinned_threads_by_order_key(vec![
            ThreadSummary {
                pin_order_key: Some("m".into()),
                ..thread("thread-2", "2026-03-09T10:00:00.000Z")
            },
            ThreadSummary {
                pin_order_key: Some("m".into()),
                ..thread("thread-1", "2026-03-09T11:00:00.000Z")
            },
        ]);
        assert_eq!(ids(&sorted), ["thread-1", "thread-2"]);
    }

    #[test]
    fn leaves_unique_insertable_keys_for_any_count() {
        for count in [0, 1, 650, 675, 676, 1_001, 2_000] {
            let keys = generate_spread_pin_order_keys(count);
            assert_eq!(keys.len(), count);
            let mut sorted = keys.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted, keys);
            for (index, after) in keys.iter().enumerate() {
                let before = index.checked_sub(1).map(|i| keys[i].as_str());
                assert!(after.bytes().all(|b| b.is_ascii_lowercase()) && !after.ends_with('a'));
                let between = pin_order_key_between(before, Some(after)).unwrap();
                assert!(&between < after);
                if let Some(before) = before {
                    assert!(between.as_str() > before);
                }
            }
        }
    }

    #[test]
    fn keeps_new_and_reopened_threads_ahead_of_the_saved_order() {
        let sorted = sort_active_threads_by_order_key(vec![
            ThreadSummary {
                active_order_key: Some("f".into()),
                ..thread("arranged-first", "2026-03-09T09:00:00.000Z")
            },
            thread("new", "2026-03-09T11:00:00.000Z"),
            ThreadSummary {
                unsettled_at: Some(ms("2026-03-09T13:00:00.000Z")),
                active_order_key: Some("t".into()),
                ..thread("arranged-last", "2026-03-09T12:00:00.000Z")
            },
            ThreadSummary {
                unsettled_at: Some(ms("2026-03-09T12:00:00.000Z")),
                ..thread("reopened", "2026-03-01T09:00:00.000Z")
            },
        ]);
        assert_eq!(
            ids(&sorted),
            ["reopened", "new", "arranged-first", "arranged-last"]
        );
    }

    #[test]
    fn breaks_equal_order_keys_and_timestamps_by_thread() {
        for key in [None, Some("m")] {
            let threads = ["thread-b", "thread-a"]
                .map(|id| ThreadSummary {
                    active_order_key: key.map(String::from),
                    ..thread(id, "2026-03-09T10:00:00.000Z")
                })
                .to_vec();
            assert_eq!(
                ids(&sort_active_threads_by_order_key(threads)),
                ["thread-a", "thread-b"]
            );
        }
    }

    #[test]
    fn applies_every_move_across_a_mixed_keyless_and_keyed_section() {
        let threads: Vec<_> = (0..6)
            .map(|index| ThreadSummary {
                active_order_key: (index >= 3).then(|| ["f", "m", "t"][index - 3].to_string()),
                ..thread(
                    &index.to_string(),
                    &format!("2026-03-09T0{}:00:00.000Z", 6 - index),
                )
            })
            .collect();
        let all = ids(&threads);
        let keys: BTreeMap<_, _> = threads
            .iter()
            .map(|thread| (thread.id.clone(), thread.active_order_key.clone()))
            .collect();
        for moved in &all {
            for target in 0..all.len() {
                let mut desired: Vec<_> = all.iter().filter(|id| *id != moved).cloned().collect();
                desired.insert(target, moved.clone());
                let next: BTreeMap<_, _> = plan_pinned_reorder(&desired, &keys, moved)
                    .into_iter()
                    .map(|a| (a.id, a.order_key))
                    .collect();
                let updated = threads
                    .iter()
                    .map(|thread| ThreadSummary {
                        active_order_key: next
                            .get(&thread.id)
                            .cloned()
                            .or(thread.active_order_key.clone()),
                        ..thread.clone()
                    })
                    .collect();
                assert_eq!(ids(&sort_active_threads_by_order_key(updated)), desired);
            }
        }
    }

    #[test]
    fn moves_a_keyless_thread_into_the_arranged_run_with_one_write() {
        let assignments = plan_pinned_move(
            &strings(&["new", "reopened", "first", "last"]),
            &keys(&[
                ("new", None),
                ("reopened", None),
                ("first", Some("f")),
                ("last", Some("t")),
            ]),
            "reopened",
            MoveDirection::Down,
        )
        .unwrap();
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].id, "reopened");
        assert!(assignments[0].order_key.as_str() > "f");
        assert!(assignments[0].order_key.as_str() < "t");
    }

    #[test]
    fn materializes_a_large_active_list_without_changing_the_requested_order() {
        let threads: Vec<_> = (0..1_200)
            .map(|index| thread(&index.to_string(), "2026-03-09T10:00:00.000Z"))
            .collect();
        let ordered: Vec<_> = ids(&threads).into_iter().rev().collect();
        let keys = threads
            .iter()
            .map(|thread| (thread.id.clone(), None))
            .collect();
        let next: BTreeMap<_, _> = plan_pinned_reorder(&ordered, &keys, &ordered[0])
            .into_iter()
            .map(|a| (a.id, a.order_key))
            .collect();
        let updated = threads
            .into_iter()
            .map(|thread| ThreadSummary {
                active_order_key: next.get(&thread.id).cloned(),
                ..thread
            })
            .collect();
        assert_eq!(ids(&sort_active_threads_by_order_key(updated)), ordered);
    }
}
