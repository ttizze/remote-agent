//! Child clocks use the known full wall span, rather than summing turns.
use crate::{
    localization::{UiLanguage, localized_ui_format},
    models::{Thread, Turn, TurnStatus},
};

fn turn_start(turn: &Turn) -> Option<f64> {
    turn.started_at_ms
        .map(|ms| ms as f64)
        .or_else(|| turn.started_at.map(|seconds| seconds * 1000.0))
        .filter(|ms| ms.is_finite())
}

fn child_times(child: Option<&Thread>) -> (Option<f64>, Option<f64>) {
    let Some(child) = child.filter(|child| child.history_has_more != Some(true)) else {
        return (None, None);
    };
    let Some(turns) = child.turns.as_ref().filter(|turns| !turns.is_empty()) else {
        return (None, None);
    };
    let start = turns
        .iter()
        .filter_map(|turn| turn_start(turn))
        .min_by(f64::total_cmp);
    // Any unfinished/unknown end prevents a settled clock from underreporting.
    let end = turns.iter().try_fold(f64::NEG_INFINITY, |end, turn| {
        let ms = turn
            .completed_at_ms
            .map(|ms| ms as f64)
            .or_else(|| Some(turn_start(turn)? + turn.duration_ms? as f64))
            .filter(|ms| ms.is_finite())?;
        Some(end.max(ms))
    });
    (start, end)
}

pub fn subagent_elapsed(
    language: UiLanguage,
    child: Option<&Thread>,
    now_seconds: f64,
) -> Option<String> {
    let child = child.filter(|child| child.history_has_more != Some(true))?;
    let turns = child.turns.as_ref()?;
    let (start, end) = child_times(Some(child));
    let end = if turns.iter().any(|turn| turn.status == TurnStatus::Running) {
        Some(now_seconds * 1000.0)
    } else {
        end
    };
    let elapsed = match (start, end) {
        (Some(start), Some(end)) => finite_elapsed(start, end)?,
        _ if turns.len() == 1 => turns[0].duration_ms?,
        _ => return None,
    };
    Some(format_elapsed(language, elapsed))
}

fn finite_elapsed(start: f64, end: f64) -> Option<u64> {
    let elapsed = end - start;
    elapsed.is_finite().then_some(elapsed.max(0.0) as u64)
}

fn format_elapsed(language: UiLanguage, elapsed: u64) -> String {
    let seconds = elapsed / 1000;
    let parts = if seconds < 60 {
        vec![(seconds.to_string(), "duration.seconds")]
    } else if seconds < 3600 {
        vec![
            ((seconds / 60).to_string(), "duration.minutes"),
            (format!("{:02}", seconds % 60), "duration.seconds"),
        ]
    } else {
        vec![
            ((seconds / 3600).to_string(), "duration.hours"),
            (format!("{:02}", seconds / 60 % 60), "duration.minutes"),
        ]
    };
    parts
        .into_iter()
        .map(|(value, key)| localized_ui_format(language, key.into(), vec![value]))
        .collect::<Vec<_>>()
        .join(" ")
}

pub struct SubagentGroupPresentation {
    pub summary: String,
    pub elapsed: Option<String>,
    pub active: bool,
    pub failed: bool,
}

/// As the source group, live children use now; a settled group needs every end.
pub fn subagent_group<'a>(
    language: UiLanguage,
    agents: impl IntoIterator<Item = (TurnStatus, Option<&'a Thread>)>,
    now_seconds: f64,
) -> SubagentGroupPresentation {
    let mut counts = [0usize; 5];
    let mut start: Option<f64> = None;
    let mut end: Option<f64> = None;
    let mut end_unknown = false;
    for (status, child) in agents {
        counts[match status {
            TurnStatus::Running => 0,
            TurnStatus::Completed => 1,
            TurnStatus::Failed => 2,
            TurnStatus::Interrupted => 3,
            TurnStatus::Unknown => 4,
        }] += 1;
        let (child_start, child_end) = child_times(child);
        if let Some(ms) = child_start {
            start = Some(start.map_or(ms, |start| start.min(ms)));
        }
        if let Some(ms) = child_end {
            end = Some(end.map_or(ms, |end| end.max(ms)));
        } else {
            end_unknown = true;
        }
    }
    let active = counts[0] > 0;
    let summary = counts
        .into_iter()
        .zip([
            "subagent.group.working",
            "subagent.group.done",
            "subagent.group.failed",
            "subagent.group.stopped",
            "subagent.group.unknown",
        ])
        .filter(|(count, _)| *count > 0)
        .map(|(count, key)| localized_ui_format(language, key.into(), vec![count.to_string()]))
        .collect::<Vec<_>>()
        .join(" · ");
    let end = if active {
        Some(now_seconds * 1000.0)
    } else if end_unknown {
        None
    } else {
        end
    };
    let elapsed = start
        .zip(end)
        .and_then(|(start, end)| finite_elapsed(start, end))
        .map(|elapsed| format_elapsed(language, elapsed));
    SubagentGroupPresentation {
        summary,
        elapsed,
        active,
        failed: counts[2] > 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn child(start: u64, end: Option<u64>) -> Thread {
        Thread {
            turns: Some(vec![Arc::new(Turn {
                status: if end.is_some() {
                    TurnStatus::Completed
                } else {
                    TurnStatus::Running
                },
                started_at_ms: Some(start),
                completed_at_ms: end,
                ..Default::default()
            })]),
            ..Default::default()
        }
    }

    #[test]
    fn groups_count_each_status_once_and_use_live_child_wall_time_then_freeze() {
        let early = child(20_000, Some(30_000));
        let late = child(40_000, Some(81_000));
        let statuses = [
            TurnStatus::Unknown,
            TurnStatus::Interrupted,
            TurnStatus::Failed,
            TurnStatus::Completed,
            TurnStatus::Running,
            TurnStatus::Completed,
        ];
        let group = subagent_group(
            UiLanguage::English,
            statuses.map(|status| (status, Some(&early))),
            90.9,
        );
        assert_eq!(
            group.summary,
            "1 working · 2 done · 1 failed · 1 stopped · 1 unknown"
        );
        assert_eq!(group.elapsed.as_deref(), Some("1m 10s"));
        assert!(group.active && group.failed);
        let stopped = [
            (TurnStatus::Completed, Some(&early)),
            (TurnStatus::Interrupted, Some(&late)),
        ];
        for now in [100.0, 200.0, f64::NAN] {
            let group = subagent_group(UiLanguage::English, stopped, now);
            assert_eq!(group.summary, "1 done · 1 stopped");
            assert_eq!(group.elapsed.as_deref(), Some("1m 01s"));
            assert!(!group.active && !group.failed);
        }
        let group = subagent_group(UiLanguage::Japanese, stopped, 100.0);
        assert_eq!(group.summary, "1件完了 · 1件中断");
        assert_eq!(group.elapsed.as_deref(), Some("1分 01秒"));
    }

    #[test]
    fn unknown_child_ends_never_shorten_a_settled_group_clock() {
        let known = child(10_000, Some(30_000));
        let mut unknown = child(20_000, None);
        Arc::make_mut(&mut unknown.turns.as_mut().unwrap()[0]).status = TurnStatus::Completed;
        for missing in [None, Some(&unknown), Some(&Thread::default())] {
            let group = subagent_group(
                UiLanguage::English,
                [
                    (TurnStatus::Completed, Some(&known)),
                    (TurnStatus::Completed, missing),
                ],
                100.0,
            );
            assert!(group.elapsed.is_none());
            let group = subagent_group(
                UiLanguage::English,
                [
                    (TurnStatus::Completed, Some(&known)),
                    (TurnStatus::Running, missing),
                ],
                100.0,
            );
            assert_eq!(group.elapsed.as_deref(), Some("1m 30s"));
        }
        let mut combined = known.clone();
        combined
            .turns
            .as_mut()
            .unwrap()
            .extend(unknown.turns.unwrap());
        assert!(subagent_elapsed(UiLanguage::English, Some(&combined), 100.0).is_none());
        combined.history_has_more = Some(true);
        assert!(
            subagent_group(
                UiLanguage::English,
                [(TurnStatus::Completed, Some(&combined))],
                100.0
            )
            .elapsed
            .is_none()
        );
    }

    #[test]
    fn missing_starts_nonfinite_clocks_empty_groups_and_clock_skew_remain_truthful() {
        let unknown = Thread::default();
        let empty = subagent_group(UiLanguage::English, [], 100.0);
        assert!(
            empty.summary.is_empty() && empty.elapsed.is_none() && !empty.active && !empty.failed
        );
        assert!(
            subagent_group(
                UiLanguage::English,
                [(TurnStatus::Running, Some(&unknown))],
                100.0
            )
            .elapsed
            .is_none()
        );
        let mut known = child(10_000, Some(20_000));
        for now in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                subagent_group(
                    UiLanguage::English,
                    [(TurnStatus::Running, Some(&known))],
                    now
                )
                .elapsed
                .is_none()
            );
        }
        let skew = subagent_group(
            UiLanguage::English,
            [(TurnStatus::Running, Some(&known))],
            5.0,
        );
        assert_eq!(skew.elapsed.as_deref(), Some("0s"));
        Arc::make_mut(&mut known.turns.as_mut().unwrap()[0]).started_at_ms = None;
        assert!(
            subagent_group(
                UiLanguage::English,
                [(TurnStatus::Completed, Some(&known))],
                100.0
            )
            .elapsed
            .is_none()
        );
    }
}
