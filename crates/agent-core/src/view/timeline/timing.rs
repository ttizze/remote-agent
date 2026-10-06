//! Durations of turns and delegated work.
use agent_domain::{ItemStatus, ThreadShell, Timestamp};

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

/// When the working indicator counts from: the start of the work the
/// activity-owning run does. A local send counts only until the Host names a run.
pub fn working_started_at(
    shell: Option<&ThreadShell>,
    send_started_at: Option<&Timestamp>,
) -> Option<Timestamp> {
    shell
        .and_then(|shell| shell.activity_run_started_at.clone())
        .or_else(|| {
            shell
                .is_none_or(|shell| shell.active_run.is_none())
                .then(|| send_started_at.cloned())
                .flatten()
        })
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

    fn shell(activity: Option<&str>, active_run: Option<&str>) -> ThreadShell {
        let mut shell = agent_domain::shell(&crate::sync::fixtures::thread_state("t")).unwrap();
        shell.activity_run_started_at = activity.map(at);
        shell.active_run = active_run.map(|run| agent_domain::RunId::new(run).unwrap());
        shell
    }

    #[test]
    fn shares_the_detail_timer_when_a_newer_run_is_queued_or_cancelled() {
        let activity = "2026-03-09T10:00:00.000Z";
        let shell = shell(Some(activity), Some("older-run"));
        for send in ["2026-03-09T10:30:00.000Z", "2026-03-09T10:50:00.000Z"] {
            assert_eq!(
                working_started_at(Some(&shell), Some(&at(send))),
                Some(at(activity))
            );
        }
        // A Host-owned run without a valid start must not borrow a local dispatch clock.
        let without_start = self::shell(None, Some("older-run"));
        assert_eq!(
            working_started_at(Some(&without_start), Some(&at("2026-03-09T10:50:00.000Z"))),
            None
        );
    }

    #[test]
    fn a_local_send_counts_until_the_host_names_a_run() {
        let send = at("2026-03-09T10:50:00.000Z");
        assert_eq!(working_started_at(None, Some(&send)), Some(send.clone()));
        assert_eq!(
            working_started_at(Some(&shell(None, None)), Some(&send)),
            Some(send.clone())
        );
        assert_eq!(working_started_at(None, None), None);
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
