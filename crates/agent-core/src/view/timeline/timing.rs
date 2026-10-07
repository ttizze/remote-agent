//! When the working indicator counts from, and elapsed time between instants.
use agent_domain::{ThreadShell, Timestamp};

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

/// The timeline's working row: "Setting up worktree…" while the worktree is
/// prepared, "Compacting…" during a compaction, else "Working for 12s" from
/// the row's start, or "Working..." before one is known.
pub fn working_row_label(
    started_at: Option<&Timestamp>,
    now_ms: i64,
    preparing_worktree: bool,
    compacting: bool,
) -> String {
    if preparing_worktree {
        return "Setting up worktree…".into();
    }
    if compacting {
        return "Compacting…".into();
    }
    let Some(started_at) = started_at else {
        return "Working...".into();
    };
    let seconds = (now_ms - started_at.millis()).max(0) / 1_000;
    let timer = if seconds < 60 {
        format!("{seconds}s")
    } else {
        crate::view::time::format_duration(seconds * 1_000)
    };
    format!("Working for {timer}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_working_row_counts_whole_seconds_from_its_start() {
        let start = at("2026-03-09T10:00:00.000Z");
        let after = |ms: i64| start.millis() + ms;
        assert_eq!(
            working_row_label(Some(&start), after(12_900), false, false),
            "Working for 12s"
        );
        assert_eq!(
            working_row_label(Some(&start), after(65_900), false, false),
            "Working for 1m 5s"
        );
        assert_eq!(
            working_row_label(Some(&start), after(-5_000), false, false),
            "Working for 0s"
        );
        assert_eq!(working_row_label(None, 0, false, false), "Working...");
        assert_eq!(
            working_row_label(Some(&start), 0, true, true),
            "Setting up worktree…"
        );
        assert_eq!(working_row_label(None, 0, false, true), "Compacting…");
    }

    fn at(value: &str) -> Timestamp {
        Timestamp::parse(value).unwrap()
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
}
