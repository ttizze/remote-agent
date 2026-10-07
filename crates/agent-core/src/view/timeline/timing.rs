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

#[cfg(test)]
mod tests {
    use super::*;

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
