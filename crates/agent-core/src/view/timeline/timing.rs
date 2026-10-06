//! Durations of turns and delegated work.
use agent_domain::{ItemStatus, RunId, RunStatus, Timestamp};

pub fn format_duration(duration_ms: f64) -> String {
    if !duration_ms.is_finite() || duration_ms < 0.0 {
        return "0ms".into();
    }
    if duration_ms < 1_000.0 {
        return format!("{}ms", duration_ms.round().max(1.0));
    }
    if duration_ms < 10_000.0 {
        let tenths = (duration_ms / 100.0).round() / 10.0;
        return if tenths >= 10.0 {
            "10s".into()
        } else {
            format!("{tenths:.1}s")
        };
    }
    if duration_ms < 60_000.0 {
        return format!("{}s", (duration_ms / 1_000.0).round());
    }
    let total_seconds = (duration_ms / 1_000.0).round() as u64;
    let (hours, minutes, seconds) = (
        total_seconds / 3_600,
        (total_seconds % 3_600) / 60,
        total_seconds % 60,
    );
    [(hours, "h"), (minutes, "m"), (seconds, "s")]
        .into_iter()
        .filter(|(value, _)| *value > 0)
        .map(|(value, unit)| format!("{value}{unit}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The milliseconds between two instants, never negative.
pub fn elapsed_ms(start: &Timestamp, end: &Timestamp) -> i64 {
    (end.millis() - start.millis()).max(0)
}

/// The run the working indicator times.
#[derive(Debug, Clone, PartialEq)]
pub struct LatestRunTiming {
    pub run: RunId,
    pub status: RunStatus,
    /// Set when the run is created; `started_at` waits for the provider.
    pub requested_at: Option<Timestamp>,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
}

/// When the working indicator counts from. A requested run counts from its
/// request while the provider starts; a settled run falls back to the send time.
pub fn active_work_started_at(
    latest: Option<&LatestRunTiming>,
    active_run: Option<&RunId>,
    send_started_at: Option<&Timestamp>,
) -> Option<Timestamp> {
    if active_run.is_some_and(|active| latest.is_none_or(|latest| &latest.run != active)) {
        return send_started_at.cloned();
    }
    if latest.is_none_or(|latest| latest.completed_at.is_none()) {
        return latest
            .and_then(|latest| latest.started_at.clone().or(latest.requested_at.clone()))
            .or_else(|| send_started_at.cloned());
    }
    send_started_at.cloned()
}

/// Unknown settled timing must not turn a task's age into its work duration.
pub fn subagent_elapsed_ms(
    status: ItemStatus,
    started_at: Option<&Timestamp>,
    completed_at: Option<&Timestamp>,
    now_ms: i64,
) -> Option<i64> {
    let start = started_at?.millis();
    let end = if status.terminal() {
        completed_at?.millis()
    } else {
        now_ms
    };
    Some((end - start).max(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> Timestamp {
        Timestamp::parse(value).unwrap()
    }
    fn run(
        id: &str,
        status: RunStatus,
        requested: &str,
        started: Option<&str>,
        completed: Option<&str>,
    ) -> LatestRunTiming {
        LatestRunTiming {
            run: RunId::new(id).unwrap(),
            status,
            requested_at: Some(at(requested)),
            started_at: started.map(at),
            completed_at: completed.map(at),
        }
    }

    #[test]
    fn formats_durations() {
        for (duration, expected) in [
            (0.0, "1ms"),
            (250.0, "250ms"),
            (1_500.0, "1.5s"),
            (9_950.0, "10s"),
            (22_000.0, "22s"),
            (60_000.0, "1m"),
            (65_000.0, "1m 5s"),
            (119_500.0, "2m"),
            (3_599_499.0, "59m 59s"),
            (3_599_500.0, "1h"),
            (3_600_000.0, "1h"),
            (3_601_000.0, "1h 1s"),
            (3_660_000.0, "1h 1m"),
            (3_661_000.0, "1h 1m 1s"),
            (7_199_500.0, "2h"),
            (25_190_000.0, "6h 59m 50s"),
            (90_061_000.0, "25h 1m 1s"),
        ] {
            assert_eq!(format_duration(duration), expected, "{duration}");
        }
    }

    #[test]
    fn handles_invalid_durations() {
        for duration in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(format_duration(duration), "0ms");
        }
    }

    #[test]
    fn does_not_time_a_superseded_turn_when_the_active_turn_differs() {
        let old = run(
            "old",
            RunStatus::Starting,
            "2026-09-06T23:33:00.000Z",
            None,
            None,
        );
        let active = RunId::new("new").unwrap();
        for send in [None, Some(at("2026-09-06T23:34:00.000Z"))] {
            assert_eq!(
                active_work_started_at(Some(&old), Some(&active), send.as_ref()),
                send
            );
        }
    }

    #[test]
    fn stops_timing_a_turn_that_failed_before_its_provider_started() {
        let failed = run(
            "turn-1",
            RunStatus::Failed,
            "2026-09-06T23:33:00.000Z",
            None,
            Some("2026-09-06T23:33:05.000Z"),
        );
        assert_eq!(active_work_started_at(Some(&failed), None, None), None);
    }

    #[test]
    fn counts_from_requested_at_while_the_provider_is_still_starting() {
        let starting = run(
            "turn-1",
            RunStatus::Starting,
            "2026-09-06T23:33:00.000Z",
            None,
            None,
        );
        assert_eq!(
            active_work_started_at(Some(&starting), None, None),
            Some(at("2026-09-06T23:33:00.000Z"))
        );
    }

    #[test]
    fn prefers_the_turns_own_started_at_once_the_provider_reports_it() {
        let running = run(
            "turn-1",
            RunStatus::Running,
            "2026-09-06T23:33:00.000Z",
            Some("2026-09-06T23:33:05.000Z"),
            None,
        );
        let active = RunId::new("turn-1").unwrap();
        assert_eq!(
            active_work_started_at(Some(&running), Some(&active), None),
            Some(at("2026-09-06T23:33:05.000Z"))
        );
    }

    #[test]
    fn stops_counting_once_the_turn_has_settled_even_while_a_session_starts_again() {
        let settled = run(
            "turn-1",
            RunStatus::Completed,
            "2026-09-06T23:33:00.000Z",
            Some("2026-09-06T23:33:05.000Z"),
            Some("2026-09-06T23:33:09.000Z"),
        );
        assert_eq!(active_work_started_at(Some(&settled), None, None), None);
    }

    #[test]
    fn falls_back_to_the_callers_send_timestamp_when_there_is_no_turn_yet() {
        let send = at("2026-09-06T23:33:00.000Z");
        assert_eq!(
            active_work_started_at(None, None, Some(&send)),
            Some(send.clone())
        );
    }

    #[test]
    fn does_not_count_the_age_of_settled_work_with_unknown_completion_timing() {
        let started = at("2026-09-21T12:00:00.000Z");
        let completed = at("2026-09-21T12:00:10.000Z");
        let now = at("2026-09-21T13:00:00.000Z").millis();
        for status in [
            ItemStatus::Completed,
            ItemStatus::Failed,
            ItemStatus::Cancelled,
            ItemStatus::Interrupted,
        ] {
            assert_eq!(subagent_elapsed_ms(status, Some(&started), None, now), None);
            assert_eq!(
                subagent_elapsed_ms(status, Some(&started), Some(&completed), now),
                Some(10_000)
            );
        }
    }

    #[test]
    fn counts_a_resumed_activation_despite_a_stale_previous_completion_timestamp() {
        let started = at("2026-09-21T12:00:00.000Z");
        let completed = at("2026-09-21T12:00:10.000Z");
        let now = at("2026-09-21T13:00:00.000Z").millis();
        assert_eq!(
            subagent_elapsed_ms(ItemStatus::Running, Some(&started), Some(&completed), now),
            Some(3_600_000)
        );
        assert_eq!(
            subagent_elapsed_ms(ItemStatus::Running, None, None, now),
            None
        );
    }
}
