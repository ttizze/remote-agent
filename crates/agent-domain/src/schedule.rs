//! Scheduled task schedules: when the next run is due, whether two schedules
//! fire at the same times, and whether a due fixed-time run arrived too late
//! to fire. Wall-clock times are read in the zone the caller supplies.
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Newly saved interval schedules run at most once per minute.
pub const MIN_SCHEDULED_TASK_INTERVAL_MS: u64 = 60_000;
/// How late a fixed-time run may fire before it counts as missed. Covers poll
/// jitter and short sleeps, while a Host started hours after the slot skips to
/// the next occurrence instead of firing stale work at a random time.
const MISSED_FIXED_TIME_GRACE_MS: i64 = 10 * 60_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Schedule {
    /// Runs repeatedly after a fixed number of milliseconds.
    Interval { every_ms: u64 },
    /// Runs at a local wall-clock time (24-hour `HH:MM`) on the listed weekdays
    /// (0 is Sunday); an empty list runs every day.
    FixedTime {
        time_of_day: String,
        #[serde(default)]
        weekdays: Vec<u8>,
    },
}

/// The hour and minute of a 24-hour `HH:MM` time; a padded hour is optional.
pub fn parse_time_of_day(value: &str) -> Option<(u32, u32)> {
    let (hour, minute) = value.trim().split_once(':')?;
    let digits = |part: &str, len: std::ops::RangeInclusive<usize>| {
        (len.contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| part.parse::<u32>().ok())
            .flatten()
    };
    let hour = digits(hour, 1..=2).filter(|hour| *hour < 24)?;
    let minute = digits(minute, 2..=2).filter(|minute| *minute < 60)?;
    Some((hour, minute))
}

/// A local wall-clock time, moved past a daylight-saving gap the way a
/// calendar does.
fn at_time<Tz: TimeZone>(timezone: &Tz, date: NaiveDate, hour: u32, minute: u32) -> Option<DateTime<Tz>> {
    let local = date.and_hms_opt(hour, minute, 0)?;
    timezone
        .from_local_datetime(&local)
        .earliest()
        .or_else(|| {
            timezone
                .from_local_datetime(&(local + Duration::hours(1)))
                .earliest()
        })
}

/// The first run after `from`; `None` when the schedule cannot fire (a
/// malformed time or an occurrence beyond the representable range).
pub fn next_run_at<Tz: TimeZone>(schedule: &Schedule, from: &DateTime<Tz>) -> Option<DateTime<Tz>> {
    match schedule {
        Schedule::Interval { every_ms } => {
            // Stored rows below the one-minute floor stay readable but must not
            // keep their old high-frequency execution rate.
            let millis = i64::try_from((*every_ms).max(MIN_SCHEDULED_TASK_INTERVAL_MS)).ok()?;
            from.clone()
                .checked_add_signed(Duration::try_milliseconds(millis)?)
        }
        Schedule::FixedTime {
            time_of_day,
            weekdays,
        } => {
            let (hour, minute) = parse_time_of_day(time_of_day)?;
            let weekdays: BTreeSet<u8> = weekdays.iter().copied().collect();
            (0..=7).find_map(|offset| {
                let date = from
                    .date_naive()
                    .checked_add_signed(Duration::days(offset))?;
                let candidate = at_time(from.timezone(), date, hour, minute)?;
                if candidate <= *from {
                    return None;
                }
                let weekday = candidate.weekday().num_days_from_sunday() as u8;
                (weekdays.is_empty() || weekdays.contains(&weekday)).then_some(candidate)
            })
        }
    }
}

/// The canonical form of a weekday mask, as `next_run_at` reads it: order and
/// duplicates are irrelevant, and an empty mask means the same as all seven
/// days.
fn weekday_key(weekdays: &[u8]) -> Option<BTreeSet<u8>> {
    let unique: BTreeSet<u8> = weekdays.iter().copied().collect();
    (!unique.is_empty() && unique.len() != 7).then_some(unique)
}

/// True iff both schedules fire at the same times. The time accepts padded
/// and unpadded hours, so the parsed time is compared, not the text.
pub fn same_schedule(a: &Schedule, b: &Schedule) -> bool {
    match (a, b) {
        (Schedule::Interval { every_ms: a }, Schedule::Interval { every_ms: b }) => a == b,
        (
            Schedule::FixedTime {
                time_of_day: a_time,
                weekdays: a_days,
            },
            Schedule::FixedTime {
                time_of_day: b_time,
                weekdays: b_days,
            },
        ) => {
            parse_time_of_day(a_time).is_some_and(|time| Some(time) == parse_time_of_day(b_time))
                && weekday_key(a_days) == weekday_key(b_days)
        }
        _ => false,
    }
}

/// True when a due fixed-time run was missed by more than the grace window and
/// should be aimed at its next occurrence instead of firing now. Interval
/// schedules are never missed: an overdue interval task catching up with a
/// single run is wanted.
pub fn missed_fixed_time_run(schedule: &Schedule, due_at_ms: i64, now_ms: i64) -> bool {
    matches!(schedule, Schedule::FixedTime { .. })
        && now_ms.saturating_sub(due_at_ms) > MISSED_FIXED_TIME_GRACE_MS
}

/// A compact human-readable label shared by settings rows and native editors.
pub fn describe_schedule(schedule: &Schedule) -> String {
    match schedule {
        Schedule::Interval { every_ms } => {
            if every_ms % 60_000 == 0 {
                let minutes = every_ms / 60_000;
                format!(
                    "Every {}",
                    if minutes == 1 {
                        "minute".to_owned()
                    } else {
                        format!("{minutes} minutes")
                    }
                )
            } else {
                let seconds = every_ms / 1_000 + u64::from(every_ms % 1_000 >= 500);
                format!("Every {seconds} seconds")
            }
        }
        Schedule::FixedTime {
            time_of_day,
            weekdays,
        } => {
            let days = if weekdays.is_empty() {
                "day"
            } else if weekdays.len() == 5 && weekdays.iter().all(|day| (1..=5).contains(day)) {
                "weekday"
            } else {
                "selected day"
            };
            format!("At {time_of_day} every {days}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, Timelike, Utc};

    fn utc(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }
    fn interval(every_ms: u64) -> Schedule {
        Schedule::Interval { every_ms }
    }
    fn fixed(time_of_day: &str, weekdays: &[u8]) -> Schedule {
        Schedule::FixedTime {
            time_of_day: time_of_day.into(),
            weekdays: weekdays.to_vec(),
        }
    }

    #[test]
    fn parses_24_hour_times() {
        assert_eq!(parse_time_of_day("09:30"), Some((9, 30)));
        assert_eq!(parse_time_of_day("23:59"), Some((23, 59)));
        assert_eq!(parse_time_of_day("25:00"), None);
        assert_eq!(parse_time_of_day("9:5"), None);
    }

    #[test]
    fn calculates_interval_schedules_from_the_supplied_instant() {
        let next = next_run_at(&interval(5 * 60_000), &utc("2026-07-01T16:00:00Z"));
        assert_eq!(next, Some(utc("2026-07-01T16:05:00Z")));
    }

    #[test]
    fn clamps_sub_minute_intervals_to_the_one_minute_execution_floor() {
        let next = next_run_at(&interval(1_000), &utc("2026-07-01T16:00:00Z"));
        assert_eq!(next, Some(utc("2026-07-01T16:01:00Z")));
    }

    #[test]
    fn an_interval_beyond_the_representable_range_has_no_next_run() {
        assert_eq!(
            next_run_at(
                &interval(9_000_000_000_000_000),
                &utc("2026-07-01T16:00:00Z")
            ),
            None
        );
    }

    #[test]
    fn skips_to_the_next_matching_fixed_time_weekday() {
        // Friday 10:00 in Los Angeles (summer: UTC-7).
        let zone = FixedOffset::west_opt(7 * 3600).unwrap();
        let from = zone.with_ymd_and_hms(2026, 7, 3, 10, 0, 0).unwrap();
        let next = next_run_at(&fixed("09:00", &[1, 2, 3, 4, 5]), &from).unwrap();
        assert_eq!(next.weekday().num_days_from_sunday(), 1);
        assert_eq!((next.hour(), next.minute()), (9, 0));
        assert_eq!(next.date_naive().to_string(), "2026-07-06");
    }

    #[test]
    fn skips_fixed_time_runs_missed_by_more_than_the_grace_window() {
        let fixed_time = fixed("09:00", &[]);
        let due_at = utc("2026-07-01T09:00:00Z").timestamp_millis();
        let within_grace = utc("2026-07-01T09:05:00Z").timestamp_millis();
        let past_grace = utc("2026-07-01T15:00:00Z").timestamp_millis();
        // A run only slightly late (poll jitter, short sleep) still fires.
        assert!(!missed_fixed_time_run(&fixed_time, due_at, within_grace));
        // A run hours past its slot is skipped and rescheduled instead.
        assert!(missed_fixed_time_run(&fixed_time, due_at, past_grace));
        // Interval schedules always catch up with a single run, never skip.
        assert!(!missed_fixed_time_run(
            &interval(60_000),
            due_at,
            past_grace
        ));
    }

    #[test]
    fn compares_schedules_structurally() {
        assert!(same_schedule(&interval(60_000), &interval(60_000)));
        assert!(!same_schedule(&interval(60_000), &interval(30_000)));
        assert!(same_schedule(
            &fixed("09:00", &[1, 2]),
            &fixed("09:00", &[1, 2])
        ));
        // Weekday masks are sets: order and duplicates do not change firing.
        assert!(same_schedule(
            &fixed("09:00", &[5, 1]),
            &fixed("09:00", &[1, 5, 5])
        ));
        // Empty and all-seven masks both mean daily.
        assert!(same_schedule(
            &fixed("09:00", &[]),
            &fixed("09:00", &[0, 1, 2, 3, 4, 5, 6])
        ));
        assert!(!same_schedule(
            &fixed("09:00", &[1, 2]),
            &fixed("09:00", &[1, 3])
        ));
        assert!(!same_schedule(&fixed("09:00", &[]), &fixed("09:30", &[])));
        assert!(!same_schedule(&interval(60_000), &fixed("09:00", &[])));
    }

    #[test]
    fn treats_padded_and_unpadded_hours_as_the_same_fixed_time_schedule() {
        for (before, after) in [("9:00", "09:00"), ("09:00", "9:00"), ("0:30", "00:30")] {
            assert!(same_schedule(&fixed(before, &[]), &fixed(after, &[])));
        }
    }

    #[test]
    fn serializes_with_the_schedule_type_tag() {
        assert_eq!(
            serde_json::to_string(&interval(60_000)).unwrap(),
            r#"{"type":"interval","everyMs":60000}"#
        );
        assert_eq!(
            serde_json::to_string(&fixed("09:00", &[1, 5])).unwrap(),
            r#"{"type":"fixed_time","timeOfDay":"09:00","weekdays":[1,5]}"#
        );
    }

    #[test]
    fn describes_interval_and_daily_schedules() {
        assert_eq!(
            describe_schedule(&interval(60_000)),
            "Every minute"
        );
        assert_eq!(
            describe_schedule(&interval(90_000)),
            "Every 90 seconds"
        );
        assert_eq!(describe_schedule(&fixed("09:00", &[])), "At 09:00 every day");
        assert_eq!(
            describe_schedule(&fixed("09:00", &[1, 2, 3, 4, 5])),
            "At 09:00 every weekday"
        );
        assert_eq!(
            describe_schedule(&fixed("09:00", &[1, 4])),
            "At 09:00 every selected day"
        );
    }
}
