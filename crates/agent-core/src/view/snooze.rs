//! Snooze rules: when a snoozed thread hides, raises its hand or woke, the
//! preset and custom wake times, and their labels.
use super::thread_summary::{RuntimeStatus, ThreadSummary, iso};
use super::time::{TimestampFormat, resolve_local, short_month_day, short_weekday, time_of_day};
use chrono::{DateTime, Datelike, Days, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};

/// A user message no run adopted within this window is a failed start, not
/// pending work.
pub const QUEUED_TURN_START_GRACE_MS: i64 = 2 * 60 * 1_000;
const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MINUTE_MS;
const DAY_MS: i64 = 24 * HOUR_MS;
const EVENING_HOUR: u32 = 18;
const MORNING_HOUR: u32 = 9;

/// A user message no run has picked up yet, within the adoption window.
pub fn has_queued_turn_start(thread: &ThreadSummary, now_ms: i64) -> bool {
    if matches!(
        thread.runtime_status(),
        Some(RuntimeStatus::Preparing | RuntimeStatus::Queued | RuntimeStatus::Starting)
    ) {
        return true;
    }
    let Some(message_at) = thread.latest_user_message_at else {
        return false;
    };
    if (now_ms - message_at).abs() > QUEUED_TURN_START_GRACE_MS {
        return false;
    }
    let Some(run) = &thread.latest_run else {
        return true;
    };
    [run.requested_at, run.started_at, run.completed_at]
        .into_iter()
        .flatten()
        .all(|at| at < message_at)
}

fn completed_after_snooze(thread: &ThreadSummary) -> Option<i64> {
    let snoozed_at = thread.snoozed_at?;
    let run = thread.latest_run.as_ref()?;
    if run.status != RuntimeStatus::Completed {
        return None;
    }
    run.completed_at.filter(|completed| *completed > snoozed_at)
}

/// A snoozed thread raises its hand when the agent is blocked on the user, it
/// failed after the snooze, or a run completed after the snooze.
pub fn raised_hand_while_snoozed(thread: &ThreadSummary) -> bool {
    if thread.has_pending_approvals || thread.has_pending_user_input {
        return true;
    }
    if let Some(runtime) = &thread.runtime
        && runtime.status == RuntimeStatus::Failed
        && thread
            .snoozed_at
            .is_none_or(|snoozed_at| runtime.updated_at > snoozed_at)
    {
        return true;
    }
    completed_after_snooze(thread).is_some()
}

/// Snooze only hides; it is refused while the agent waits on the user or a
/// queued start is invisible pending work.
pub fn can_snooze(thread: &ThreadSummary, now_ms: i64) -> bool {
    !(thread.has_pending_approvals
        || thread.has_pending_user_input
        || has_queued_turn_start(thread, now_ms))
}

/// Hidden while the wake time is ahead and the thread has not raised its hand.
pub fn effective_snoozed(thread: &ThreadSummary, now_ms: i64) -> bool {
    thread
        .snoozed_until
        .is_some_and(|wake| wake > now_ms && !raised_hand_while_snoozed(thread))
}

/// When a snoozed thread woke: the trigger of an early wake, otherwise the
/// elapsed wake time.
pub fn thread_woke_at(thread: &ThreadSummary, now_ms: i64) -> Option<i64> {
    let wake = thread.snoozed_until?;
    if raised_hand_while_snoozed(thread) {
        return completed_after_snooze(thread).or_else(|| {
            thread
                .runtime
                .as_ref()
                .map(|runtime| runtime.updated_at)
                .or(thread.snoozed_at)
        });
    }
    (wake <= now_ms).then_some(wake)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SnoozePresetId {
    Hour,
    ThreeHours,
    Evening,
    Tomorrow,
    NextWeek,
}
impl SnoozePresetId {
    pub fn key(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::ThreeHours => "three-hours",
            Self::Evening => "evening",
            Self::Tomorrow => "tomorrow",
            Self::NextWeek => "next-week",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SnoozePreset {
    pub id: SnoozePresetId,
    pub label: String,
    /// The time column; it complements the label instead of repeating it.
    pub when_label: String,
    pub snoozed_until: String,
}

fn at_hour<Tz: TimeZone>(base: &DateTime<Tz>, hour: u32) -> DateTime<Tz> {
    let local = base
        .date_naive()
        .and_time(NaiveTime::from_hms_opt(hour, 0, 0).expect("valid hour"));
    resolve_local(&base.timezone(), local).unwrap_or_else(|| base.clone())
}

/// Calendar days, so a daylight-saving day does not skip a date.
fn add_days<Tz: TimeZone>(base: &DateTime<Tz>, days: u64) -> DateTime<Tz> {
    let local = base.naive_local() + Days::new(days);
    resolve_local(&base.timezone(), local).unwrap_or_else(|| base.clone())
}

fn preset<Tz: TimeZone>(
    id: SnoozePresetId,
    label: &str,
    when_label: String,
    wake: &DateTime<Tz>,
) -> SnoozePreset {
    SnoozePreset {
        id,
        label: label.into(),
        when_label,
        snoozed_until: iso(wake.timestamp_millis()),
    }
}

/// "This evening" shows only while evening is over an hour away; "Next week"
/// drops when it lands on the same Monday as "Tomorrow".
pub fn resolve_snooze_presets<Tz: TimeZone>(
    now: &DateTime<Tz>,
    format: TimestampFormat,
) -> Vec<SnoozePreset> {
    let in_hours = |hours: i64| now.clone() + chrono::Duration::milliseconds(hours * HOUR_MS);
    let hour = in_hours(1);
    let three_hours = in_hours(3);
    let mut presets = vec![
        preset(
            SnoozePresetId::Hour,
            "In 1 hour",
            time_of_day(&hour, format),
            &hour,
        ),
        preset(
            SnoozePresetId::ThreeHours,
            "In 3 hours",
            time_of_day(&three_hours, format),
            &three_hours,
        ),
    ];
    let evening = at_hour(now, EVENING_HOUR);
    if evening.timestamp_millis() - now.timestamp_millis() > HOUR_MS {
        presets.push(preset(
            SnoozePresetId::Evening,
            "This evening",
            time_of_day(&evening, format),
            &evening,
        ));
    }
    let tomorrow = at_hour(&add_days(now, 1), MORNING_HOUR);
    presets.push(preset(
        SnoozePresetId::Tomorrow,
        "Tomorrow",
        time_of_day(&tomorrow, format),
        &tomorrow,
    ));
    let weekday = now.weekday().num_days_from_sunday();
    let days_until_monday = match (8 - weekday) % 7 {
        0 => 7,
        days => days,
    };
    let next_week = at_hour(&add_days(now, u64::from(days_until_monday)), MORNING_HOUR);
    if next_week != tomorrow {
        presets.push(preset(
            SnoozePresetId::NextWeek,
            "Next week",
            format!(
                "{} {}",
                short_weekday(&next_week),
                time_of_day(&next_week, format)
            ),
            &next_week,
        ));
    }
    presets
}

/// Compact "wakes in" label: minutes round up so a hidden row never reads "0m".
pub fn snooze_wake_label(snoozed_until_ms: i64, now_ms: i64) -> String {
    let remaining = snoozed_until_ms - now_ms;
    if remaining <= 0 {
        return "now".into();
    }
    let ceil = |unit: i64| (remaining + unit - 1) / unit;
    if remaining < HOUR_MS {
        return format!("{}m", ceil(MINUTE_MS).max(1));
    }
    if remaining < DAY_MS {
        return format!("{}h", ceil(HOUR_MS));
    }
    format!("{}d", ceil(DAY_MS))
}

/// Menus and toasts: "18:00" today, "tomorrow 9:00", "Mon 9:00", "Apr 20, 9:00".
pub fn snooze_wake_description<Tz: TimeZone>(
    snoozed_until_ms: i64,
    now: &DateTime<Tz>,
    format: TimestampFormat,
) -> String {
    let zone = now.timezone();
    let Some(wake) = zone.timestamp_millis_opt(snoozed_until_ms).single() else {
        return String::new();
    };
    let time = time_of_day(&wake, format);
    let start_of_today = at_midnight(now);
    let day_delta =
        (wake.timestamp_millis() - start_of_today.timestamp_millis()).div_euclid(DAY_MS);
    match day_delta {
        0 => time,
        1 => format!("tomorrow {time}"),
        delta if delta < 7 => format!("{} {time}", short_weekday(&wake)),
        _ => format!("{}, {time}", short_month_day(&wake)),
    }
}

fn at_midnight<Tz: TimeZone>(date: &DateTime<Tz>) -> DateTime<Tz> {
    resolve_local(&date.timezone(), date.date_naive().and_time(NaiveTime::MIN))
        .unwrap_or_else(|| date.clone())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SnoozeDurationUnit {
    Minutes,
    Hours,
    Days,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum CustomSnoozeInput {
    /// Local `YYYY-MM-DD` and `HH:MM`.
    Date { date: String, time: String },
    Duration {
        amount: String,
        unit: SnoozeDurationUnit,
    },
}

fn digits(value: &str, pattern: &[usize]) -> bool {
    let parts: Vec<_> = value.split(['-', ':']).collect();
    parts.len() == pattern.len()
        && parts
            .iter()
            .zip(pattern)
            .all(|(part, len)| part.len() == *len && part.bytes().all(|b| b.is_ascii_digit()))
        && value.len() == pattern.iter().sum::<usize>() + pattern.len() - 1
}

/// A future wake time from local calendar input or an elapsed duration;
/// `None` for invalid, nonexistent or past times.
pub fn resolve_custom_snooze<Tz: TimeZone>(
    input: &CustomSnoozeInput,
    now: &DateTime<Tz>,
) -> Option<String> {
    let now_ms = now.timestamp_millis();
    let wake_ms = match input {
        CustomSnoozeInput::Duration { amount, unit } => {
            let amount: f64 = amount.trim().parse().ok()?;
            if !amount.is_finite() || amount <= 0.0 {
                return None;
            }
            let unit_ms = match unit {
                SnoozeDurationUnit::Minutes => MINUTE_MS,
                SnoozeDurationUnit::Hours => HOUR_MS,
                SnoozeDurationUnit::Days => DAY_MS,
            };
            let wake = now_ms as f64 + amount * unit_ms as f64;
            if !wake.is_finite() || wake.abs() > 8.64e15 {
                return None;
            }
            wake.trunc() as i64
        }
        CustomSnoozeInput::Date { date, time } => {
            if !digits(date, &[4, 2, 2]) || !digits(time, &[2, 2]) {
                return None;
            }
            let day = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
            let clock = NaiveTime::parse_from_str(time, "%H:%M").ok()?;
            now.timezone()
                .from_local_datetime(&day.and_time(clock))
                .earliest()?
                .timestamp_millis()
        }
    };
    (wake_ms > now_ms)
        .then(|| iso(wake_ms))
        .filter(|wake| !wake.is_empty())
}

pub fn local_snooze_date<Tz: TimeZone>(date: &DateTime<Tz>) -> String {
    format!("{:04}-{:02}-{:02}", date.year(), date.month(), date.day())
}

pub fn local_snooze_time<Tz: TimeZone>(date: &DateTime<Tz>) -> String {
    format!("{:02}:{:02}", date.hour(), date.minute())
}

/// Date pickers that exchange calendar days at UTC midnight receive the
/// local day unchanged.
pub fn snooze_date_to_picker_date<Tz: TimeZone>(date: &DateTime<Tz>) -> String {
    let midnight = Utc.from_utc_datetime(&date.date_naive().and_time(NaiveTime::MIN));
    iso(midnight.timestamp_millis())
}

/// Applies a picker's UTC calendar day, keeping the local time.
pub fn apply_snooze_picker_date<Tz: TimeZone>(
    date: &DateTime<Tz>,
    selected: &DateTime<Utc>,
) -> Option<DateTime<Tz>> {
    let day = selected.date_naive();
    resolve_local(&date.timezone(), day.and_time(date.time()))
}

/// Applies a picked local time to the chosen day, dropping seconds.
pub fn apply_snooze_picker_time<Tz: TimeZone>(
    date: &DateTime<Tz>,
    selected: &DateTime<Tz>,
) -> Option<DateTime<Tz>> {
    let time = NaiveTime::from_hms_opt(selected.hour(), selected.minute(), 0)?;
    resolve_local(&date.timezone(), date.date_naive().and_time(time))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::thread_summary::{
        RunSummary, RuntimeSummary,
        fixtures::{ms, run, runtime, summary},
    };
    use crate::view::time::fixtures::{local, with_zone};
    use chrono::Weekday;

    const NOW: &str = "2026-04-10T12:00:00.000Z";
    const SNOOZED_AT: &str = "2026-04-10T09:00:00.000Z";
    const FUTURE_WAKE: &str = "2026-04-11T09:00:00.000Z";
    const PAST_WAKE: &str = "2026-04-10T10:00:00.000Z";

    #[derive(Default)]
    struct Shell<'a> {
        snoozed_until: Option<&'a str>,
        snoozed_at: Option<&'a str>,
        runtime: Option<RuntimeStatus>,
        pending: Option<&'a str>,
        completed_at: Option<&'a str>,
    }
    /// The earlier session states map to the runtime the list now reads:
    /// an errored session is a failed runtime stamped at 11:00.
    fn shell(input: Shell) -> ThreadSummary {
        ThreadSummary {
            snoozed_until: input.snoozed_until.map(ms),
            snoozed_at: input
                .snoozed_at
                .or(input.snoozed_until.map(|_| SNOOZED_AT))
                .map(ms),
            has_pending_approvals: input.pending == Some("approval"),
            has_pending_user_input: input.pending == Some("user-input"),
            runtime: input.runtime.map(|status| RuntimeSummary {
                updated_at: ms("2026-04-10T11:00:00.000Z"),
                ..runtime(status)
            }),
            latest_run: input.completed_at.map(|completed| RunSummary {
                requested_at: Some(ms(SNOOZED_AT)),
                completed_at: Some(ms(completed)),
                ..run("turn-1", RuntimeStatus::Completed)
            }),
            ..summary("thread-1")
        }
    }
    fn now() -> i64 {
        ms(NOW)
    }

    #[test]
    fn hides_a_thread_whose_wake_time_is_in_the_future() {
        let thread = shell(Shell {
            snoozed_until: Some(FUTURE_WAKE),
            ..Shell::default()
        });
        assert!(effective_snoozed(&thread, now()));
    }

    #[test]
    fn stops_classifying_as_snoozed_once_the_wake_time_passes() {
        let thread = shell(Shell {
            snoozed_until: Some(PAST_WAKE),
            ..Shell::default()
        });
        assert!(!effective_snoozed(&thread, now()));
    }

    #[test]
    fn never_snoozes_a_thread_with_no_snooze_state() {
        assert!(!effective_snoozed(&shell(Shell::default()), now()));
    }

    #[test]
    fn wakes_early_when_the_agent_is_blocked_on_the_user() {
        for pending in ["approval", "user-input"] {
            let thread = shell(Shell {
                snoozed_until: Some(FUTURE_WAKE),
                pending: Some(pending),
                ..Shell::default()
            });
            assert!(!effective_snoozed(&thread, now()));
        }
    }

    #[test]
    fn wakes_early_on_a_failure_that_happened_after_the_snooze() {
        let thread = shell(Shell {
            snoozed_until: Some(FUTURE_WAKE),
            runtime: Some(RuntimeStatus::Failed),
            ..Shell::default()
        });
        assert!(!effective_snoozed(&thread, now()));
    }

    #[test]
    fn stays_snoozed_when_the_failure_predates_the_snooze() {
        let thread = shell(Shell {
            snoozed_until: Some(FUTURE_WAKE),
            runtime: Some(RuntimeStatus::Failed),
            snoozed_at: Some("2026-04-10T11:30:00.000Z"),
            ..Shell::default()
        });
        assert!(effective_snoozed(&thread, now()));
    }

    #[test]
    fn stays_snoozed_while_the_session_keeps_working() {
        let thread = shell(Shell {
            snoozed_until: Some(FUTURE_WAKE),
            runtime: Some(RuntimeStatus::Running),
            ..Shell::default()
        });
        assert!(effective_snoozed(&thread, now()));
    }

    #[test]
    fn wakes_early_when_a_run_completes_after_the_snooze_was_set() {
        let thread = shell(Shell {
            snoozed_until: Some(FUTURE_WAKE),
            completed_at: Some("2026-04-10T10:30:00.000Z"),
            ..Shell::default()
        });
        assert!(!effective_snoozed(&thread, now()));
    }

    #[test]
    fn ignores_runs_that_completed_before_the_snooze() {
        let thread = shell(Shell {
            snoozed_until: Some(FUTURE_WAKE),
            completed_at: Some("2026-04-10T08:00:00.000Z"),
            ..Shell::default()
        });
        assert!(effective_snoozed(&thread, now()));
    }

    #[test]
    fn a_quiet_snoozed_thread_does_not_raise_its_hand() {
        let thread = shell(Shell {
            snoozed_until: Some(FUTURE_WAKE),
            ..Shell::default()
        });
        assert!(!raised_hand_while_snoozed(&thread));
    }

    #[test]
    fn approvals_input_and_failures_raise_the_hand() {
        for (pending, runtime) in [
            (Some("approval"), None),
            (Some("user-input"), None),
            (None, Some(RuntimeStatus::Failed)),
        ] {
            let thread = shell(Shell {
                snoozed_until: Some(FUTURE_WAKE),
                pending,
                runtime,
                ..Shell::default()
            });
            assert!(raised_hand_while_snoozed(&thread));
        }
    }

    #[test]
    fn allows_snoozing_quiet_and_working_threads_alike() {
        assert!(can_snooze(&shell(Shell::default()), now()));
        let working = shell(Shell {
            runtime: Some(RuntimeStatus::Running),
            ..Shell::default()
        });
        assert!(can_snooze(&working, now()));
    }

    #[test]
    fn refuses_blocked_on_you_work() {
        for pending in ["approval", "user-input"] {
            let thread = shell(Shell {
                pending: Some(pending),
                ..Shell::default()
            });
            assert!(!can_snooze(&thread, now()));
        }
    }

    #[test]
    fn refuses_a_queued_turn_start() {
        let mut thread = shell(Shell::default());
        thread.latest_user_message_at = Some(ms("2026-04-10T11:59:30.000Z"));
        assert!(!can_snooze(&thread, now()));
        thread.latest_user_message_at = Some(ms("2026-04-10T11:00:00.000Z"));
        assert!(can_snooze(&thread, now()));
    }

    #[test]
    fn expires_queued_state_after_two_minutes() {
        let thread = ThreadSummary {
            latest_user_message_at: Some(ms("2026-04-10T11:57:59.000Z")),
            ..summary("t")
        };
        assert!(!has_queued_turn_start(&thread, now()));
    }

    #[test]
    fn clears_queued_state_when_a_turn_adopts_the_message() {
        let message_at = ms("2026-04-10T11:59:00.000Z");
        let adopted = ThreadSummary {
            latest_user_message_at: Some(message_at),
            latest_run: Some(RunSummary {
                requested_at: Some(message_at),
                ..run("turn-adopted", RuntimeStatus::Running)
            }),
            ..summary("t")
        };
        assert!(!has_queued_turn_start(&adopted, now()));
    }

    #[test]
    fn bounds_future_client_clock_skew() {
        let at = |message: &str| ThreadSummary {
            latest_user_message_at: Some(ms(message)),
            ..summary("t")
        };
        assert!(!has_queued_turn_start(
            &at("2026-04-10T12:03:00.000Z"),
            now()
        ));
        assert!(has_queued_turn_start(
            &at("2026-04-10T12:01:00.000Z"),
            now()
        ));
    }

    #[test]
    fn woke_at_is_none_for_never_snoozed_and_still_snoozed_threads() {
        assert_eq!(thread_woke_at(&shell(Shell::default()), now()), None);
        let snoozed = shell(Shell {
            snoozed_until: Some(FUTURE_WAKE),
            ..Shell::default()
        });
        assert_eq!(thread_woke_at(&snoozed, now()), None);
    }

    #[test]
    fn reports_the_wake_time_for_a_timer_wake() {
        let thread = shell(Shell {
            snoozed_until: Some(PAST_WAKE),
            ..Shell::default()
        });
        assert_eq!(thread_woke_at(&thread, now()), Some(ms(PAST_WAKE)));
    }

    #[test]
    fn reports_the_completion_time_for_an_early_run_completed_wake() {
        let thread = shell(Shell {
            snoozed_until: Some(FUTURE_WAKE),
            completed_at: Some("2026-04-10T10:30:00.000Z"),
            ..Shell::default()
        });
        assert_eq!(
            thread_woke_at(&thread, now()),
            Some(ms("2026-04-10T10:30:00.000Z"))
        );
    }

    #[test]
    fn falls_back_to_session_activity_for_blocked_or_failed_early_wakes() {
        let thread = shell(Shell {
            snoozed_until: Some(FUTURE_WAKE),
            runtime: Some(RuntimeStatus::Failed),
            ..Shell::default()
        });
        assert_eq!(
            thread_woke_at(&thread, now()),
            Some(ms("2026-04-10T11:00:00.000Z"))
        );
    }

    #[test]
    fn keeps_the_early_wake_authoritative_after_the_scheduled_time_passes() {
        let thread = shell(Shell {
            snoozed_until: Some(PAST_WAKE),
            completed_at: Some("2026-04-10T09:30:00.000Z"),
            ..Shell::default()
        });
        assert_eq!(
            thread_woke_at(&thread, now()),
            Some(ms("2026-04-10T09:30:00.000Z"))
        );
    }

    #[test]
    fn formats_remaining_time_coarsely_rounding_up() {
        let now = ms("2026-06-02T00:00:00.000Z");
        assert_eq!(
            snooze_wake_label(ms("2026-06-02T00:30:00.000Z"), now),
            "30m"
        );
        assert_eq!(snooze_wake_label(ms("2026-06-02T01:30:00.000Z"), now), "2h");
        assert_eq!(snooze_wake_label(ms("2026-06-03T02:00:00.000Z"), now), "2d");
    }

    #[test]
    fn never_reads_zero_or_negative_while_still_snoozed() {
        let now = ms("2026-06-02T00:00:00.000Z");
        assert_eq!(snooze_wake_label(ms("2026-06-02T00:00:30.000Z"), now), "1m");
        assert_eq!(
            snooze_wake_label(ms("2026-06-01T23:59:59.000Z"), now),
            "now"
        );
    }

    fn ids(presets: &[SnoozePreset]) -> Vec<&'static str> {
        presets.iter().map(|preset| preset.id.key()).collect()
    }
    fn wake(presets: &[SnoozePreset], id: SnoozePresetId) -> DateTime<chrono::Local> {
        let until = &presets.iter().find(|p| p.id == id).unwrap().snoozed_until;
        chrono::Local
            .timestamp_millis_opt(ms(until))
            .single()
            .unwrap()
    }

    #[test]
    fn offers_the_shared_desktop_and_mobile_choices() {
        let presets = resolve_snooze_presets(&local(2026, 4, 8, 10, 0), TimestampFormat::Locale);
        assert_eq!(
            ids(&presets),
            ["hour", "three-hours", "evening", "tomorrow", "next-week"]
        );
        let three = presets
            .iter()
            .find(|p| p.id == SnoozePresetId::ThreeHours)
            .unwrap();
        assert_eq!(
            three.snoozed_until,
            iso(local(2026, 4, 8, 13, 0).timestamp_millis())
        );
        assert_eq!(three.label, "In 3 hours");
        let evening = presets
            .iter()
            .find(|p| p.id == SnoozePresetId::Evening)
            .unwrap();
        assert_eq!(evening.label, "This evening");
        assert_eq!(wake(&presets, SnoozePresetId::Evening).hour(), 18);
        let tomorrow = wake(&presets, SnoozePresetId::Tomorrow);
        assert_eq!((tomorrow.day(), tomorrow.hour()), (9, 9));
        let next_week = wake(&presets, SnoozePresetId::NextWeek);
        assert_eq!((next_week.weekday(), next_week.day()), (Weekday::Mon, 13));
    }

    #[test]
    fn when_labels_complement_the_label_instead_of_repeating_it() {
        let presets = resolve_snooze_presets(&local(2026, 4, 8, 10, 0), TimestampFormat::Locale);
        assert!(
            presets
                .iter()
                .all(|p| !p.when_label.to_lowercase().contains("tomorrow"))
        );
        let label = |id| &presets.iter().find(|p| p.id == id).unwrap().when_label;
        assert!(label(SnoozePresetId::Tomorrow).contains('9'));
        assert!(label(SnoozePresetId::NextWeek).contains("Mon"));
    }

    #[test]
    fn drops_the_evening_choice_once_evening_is_near_or_past() {
        for hour in [(17, 30), (21, 0)] {
            let presets =
                resolve_snooze_presets(&local(2026, 4, 8, hour.0, hour.1), TimestampFormat::Locale);
            assert_eq!(
                ids(&presets),
                ["hour", "three-hours", "tomorrow", "next-week"]
            );
        }
    }

    #[test]
    fn puts_next_week_a_full_week_out_when_today_is_monday() {
        let presets = resolve_snooze_presets(&local(2026, 4, 6, 10, 0), TimestampFormat::Locale);
        let next_week = wake(&presets, SnoozePresetId::NextWeek);
        assert_eq!((next_week.weekday(), next_week.day()), (Weekday::Mon, 13));
    }

    #[test]
    fn drops_next_week_on_sundays_when_it_matches_tomorrow() {
        let presets = resolve_snooze_presets(&local(2026, 8, 30, 7, 1), TimestampFormat::Locale);
        assert_eq!(
            ids(&presets),
            ["hour", "three-hours", "evening", "tomorrow"]
        );
        assert_eq!(
            wake(&presets, SnoozePresetId::Tomorrow).weekday(),
            Weekday::Mon
        );
    }

    #[test]
    fn formats_preset_times_with_the_selected_clock_preference() {
        let now = local(2026, 4, 8, 10, 0);
        let evening = |format| {
            resolve_snooze_presets(&now, format)
                .into_iter()
                .find(|p| p.id == SnoozePresetId::Evening)
                .unwrap()
                .when_label
        };
        assert!(evening(TimestampFormat::TwelveHour).contains("PM"));
        assert_eq!(evening(TimestampFormat::TwentyFourHour), "18:00");
    }

    #[test]
    fn describes_wakes_by_today_tomorrow_and_weekday() {
        let now = local(2026, 4, 8, 10, 0);
        let describe = |wake: DateTime<chrono::Local>, format| {
            snooze_wake_description(wake.timestamp_millis(), &now, format)
        };
        let format = TimestampFormat::Locale;
        assert!(!describe(local(2026, 4, 8, 18, 0), format).contains("tomorrow"));
        assert!(describe(local(2026, 4, 9, 9, 0), format).contains("tomorrow"));
        assert!(describe(local(2026, 4, 13, 9, 0), format).contains("Mon"));
        assert!(describe(local(2026, 4, 8, 18, 0), TimestampFormat::TwelveHour).contains("PM"));
        assert_eq!(
            describe(local(2026, 4, 8, 18, 0), TimestampFormat::TwentyFourHour),
            "18:00"
        );
    }

    fn custom_now() -> DateTime<chrono::Local> {
        local(2026, 9, 14, 14, 30)
    }
    fn date(date: &str, time: &str) -> CustomSnoozeInput {
        CustomSnoozeInput::Date {
            date: date.into(),
            time: time.into(),
        }
    }
    fn duration(amount: &str, unit: SnoozeDurationUnit) -> CustomSnoozeInput {
        CustomSnoozeInput::Duration {
            amount: amount.into(),
            unit,
        }
    }

    #[test]
    fn converts_local_date_and_time_to_an_absolute_wake_time() {
        assert_eq!(
            resolve_custom_snooze(&date("2026-09-15", "09:15"), &custom_now()),
            Some(iso(local(2026, 9, 15, 9, 15).timestamp_millis()))
        );
    }

    #[test]
    fn rejects_invalid_or_non_future_calendar_input() {
        for (day, time) in [
            ("2026-09-14", "14:30"),
            ("2026-09-13", "09:00"),
            ("2027-02-29", "09:00"),
            ("2026-13-01", "09:00"),
            ("2026-09-15", "24:00"),
            ("2026-09-15", "09:60"),
            ("", "09:00"),
            ("2026-09-15", ""),
        ] {
            assert_eq!(
                resolve_custom_snooze(&date(day, time), &custom_now()),
                None,
                "{day} {time}"
            );
        }
    }

    #[test]
    fn resolves_durations_from_the_confirmation_time() {
        for (unit, amount, elapsed) in [
            (SnoozeDurationUnit::Minutes, "45", 45 * MINUTE_MS),
            (SnoozeDurationUnit::Hours, "1.5", 90 * MINUTE_MS),
            (SnoozeDurationUnit::Days, "2", 48 * HOUR_MS),
        ] {
            let confirmed = custom_now() + chrono::Duration::minutes(5);
            assert_eq!(
                resolve_custom_snooze(&duration(amount, unit), &confirmed),
                Some(iso(confirmed.timestamp_millis() + elapsed))
            );
        }
    }

    #[test]
    fn rejects_invalid_durations() {
        for amount in ["", "0", "-1", "NaN", "Infinity", "1e300"] {
            assert_eq!(
                resolve_custom_snooze(&duration(amount, SnoozeDurationUnit::Hours), &custom_now()),
                None,
                "{amount}"
            );
        }
    }

    #[test]
    fn rejects_nonexistent_local_times_at_the_spring_dst_transition() {
        with_zone("America/Los_Angeles");
        let now = chrono::Local
            .timestamp_millis_opt(ms("2027-03-01T00:00:00Z"))
            .unwrap();
        assert_eq!(
            resolve_custom_snooze(&date("2027-03-14", "02:30"), &now),
            None
        );
    }

    #[test]
    fn treats_duration_days_as_24_hours_across_dst() {
        with_zone("America/Los_Angeles");
        let before = local(2027, 3, 13, 12, 0);
        assert_eq!(
            resolve_custom_snooze(&duration("1", SnoozeDurationUnit::Days), &before),
            Some(iso(local(2027, 3, 14, 13, 0).timestamp_millis()))
        );
    }

    #[test]
    fn formats_local_input_values_without_converting_to_utc() {
        let date = local(2026, 1, 2, 3, 4);
        assert_eq!(local_snooze_date(&date), "2026-01-02");
        assert_eq!(local_snooze_time(&date), "03:04");
    }

    #[test]
    fn passes_the_local_calendar_day_to_the_picker_without_shifting_it() {
        assert_eq!(
            snooze_date_to_picker_date(&local(2026, 9, 17, 0, 30)),
            "2026-09-17T00:00:00.000Z"
        );
    }

    #[test]
    fn applies_the_pickers_utc_calendar_day_while_retaining_the_local_time() {
        let date = local(2026, 9, 16, 23, 45);
        let selected = Utc
            .timestamp_millis_opt(ms("2026-09-20T00:00:00Z"))
            .unwrap();
        let next = apply_snooze_picker_date(&date, &selected).unwrap();
        assert_eq!(
            (
                next.year(),
                next.month(),
                next.day(),
                next.hour(),
                next.minute()
            ),
            (2026, 9, 20, 23, 45)
        );
        assert_eq!(date.day(), 16);
    }

    #[test]
    fn applies_a_picked_local_time_without_changing_the_chosen_day() {
        let date = local(2026, 9, 20, 23, 45);
        let selected = local(2026, 9, 16, 8, 15) + chrono::Duration::seconds(30);
        let next = apply_snooze_picker_time(&date, &selected).unwrap();
        assert_eq!(
            (next.day(), next.hour(), next.minute(), next.second()),
            (20, 8, 15, 0)
        );
        assert_eq!(date.hour(), 23);
    }
}
