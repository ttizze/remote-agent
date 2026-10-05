use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationOutcome {
    Completed,
    Cancelled,
    Failed,
    Updated,
    Unknown,
}
impl From<ItemStatus> for NotificationOutcome {
    fn from(status: ItemStatus) -> Self {
        match status {
            ItemStatus::Completed => Self::Completed,
            ItemStatus::Interrupted | ItemStatus::Cancelled => Self::Cancelled,
            ItemStatus::Failed => Self::Failed,
            ItemStatus::Running | ItemStatus::Waiting => Self::Updated,
            ItemStatus::Pending => Self::Unknown,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkReport {
    pub kind: BackgroundKind,
    pub label: Option<String>,
    pub outcome: NotificationOutcome,
    pub child_thread: Option<ThreadId>,
    pub exit_code: Option<i64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotificationSource {
    Native(BackgroundKind),
    Delegated { task_ids: Vec<NodeId> },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    pub source: NotificationSource,
    pub child_thread: Option<ThreadId>,
    pub outcome: NotificationOutcome,
    pub summary: String,
    pub detail: Option<String>,
}
fn label(label: Option<&str>) -> Option<String> {
    let line = label?.trim().split('\n').next()?.trim();
    if line.is_empty() {
        return None;
    }
    // The reference limit counts JavaScript UTF-16 code units.
    if line.encode_utf16().count() <= 80 {
        Some(line.into())
    } else {
        Some(format!(
            "{}…",
            String::from_utf16_lossy(&line.encode_utf16().take(79).collect::<Vec<_>>())
        ))
    }
}
fn noun(kind: BackgroundKind) -> (&'static str, &'static str) {
    match kind {
        BackgroundKind::Subagent => ("Subagent", "subagents"),
        BackgroundKind::Command => ("Command", "commands"),
        BackgroundKind::Monitor => ("Monitor", "monitors"),
        BackgroundKind::BackgroundTask => ("Background task", "background tasks"),
    }
}
fn suffix(report: &WorkReport) -> String {
    if report.kind == BackgroundKind::Command {
        report
            .exit_code
            .map(|code| format!(" (exit {code})"))
            .unwrap_or_default()
    } else {
        String::new()
    }
}
pub fn combined_outcome(
    outcomes: impl IntoIterator<Item = NotificationOutcome>,
) -> NotificationOutcome {
    let outcomes: Vec<_> = outcomes.into_iter().collect();
    if outcomes.contains(&NotificationOutcome::Failed) {
        NotificationOutcome::Failed
    } else if outcomes.contains(&NotificationOutcome::Cancelled) {
        NotificationOutcome::Cancelled
    } else if !outcomes.is_empty()
        && outcomes
            .iter()
            .all(|o| *o == NotificationOutcome::Completed)
    {
        NotificationOutcome::Completed
    } else if outcomes.contains(&NotificationOutcome::Updated) {
        NotificationOutcome::Updated
    } else {
        NotificationOutcome::Unknown
    }
}
pub fn background_notification(reports: &[WorkReport]) -> Option<Notification> {
    let first = reports.first()?;
    let outcome = combined_outcome(reports.iter().map(|r| r.outcome));
    let source = if reports.iter().any(|r| r.kind != first.kind) {
        BackgroundKind::BackgroundTask
    } else {
        first.kind
    };
    let child_thread = if reports.len() == 1 && source == BackgroundKind::Subagent {
        first.child_thread.clone()
    } else {
        None
    };
    let summary = if reports.len() == 1 {
        let verb = match outcome {
            NotificationOutcome::Failed => "failed",
            NotificationOutcome::Cancelled => "was stopped",
            NotificationOutcome::Updated if source == BackgroundKind::Monitor => {
                "reported new output"
            }
            NotificationOutcome::Updated => "updated",
            _ => "finished",
        };
        format!(
            "{}{} {verb}{}",
            noun(source).0,
            label(first.label.as_deref())
                .map(|l| format!(" \"{l}\""))
                .unwrap_or_default(),
            suffix(first)
        )
    } else {
        let verb = match outcome {
            NotificationOutcome::Failed => "failed",
            NotificationOutcome::Cancelled => "were stopped",
            NotificationOutcome::Updated => "updated",
            _ => "finished",
        };
        if reports.len() > 3 || reports.iter().all(|r| label(r.label.as_deref()).is_none()) {
            format!("{} {} {verb}", reports.len(), noun(source).1)
        } else {
            let names: Vec<_> = reports
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    format!(
                        "{}{}{}",
                        if i == 0 {
                            noun(r.kind).0.into()
                        } else {
                            noun(r.kind).0.to_lowercase()
                        },
                        label(r.label.as_deref())
                            .map(|l| format!(" \"{l}\""))
                            .unwrap_or_default(),
                        suffix(r)
                    )
                })
                .collect();
            format!(
                "{} and {} {verb}",
                names[..names.len() - 1].join(", "),
                names.last().unwrap()
            )
        }
    };
    Some(Notification {
        detail: None,
        source: NotificationSource::Native(source),
        child_thread,
        outcome,
        summary,
    })
}
/// The notice for a rejected usage window, with the remaining wait when it
/// is within 30 days.
pub fn usage_limit_notice(limit: Option<&str>, resets_at: Option<i64>, now_ms: i64) -> String {
    let label = match limit {
        Some("five_hour") => Some("5-hour"),
        Some("seven_day") => Some("7-day"),
        Some("seven_day_opus") => Some("7-day Opus"),
        Some("seven_day_sonnet") => Some("7-day Sonnet"),
        Some("seven_day_overage_included") => Some("7-day model"),
        Some("overage") => Some("overage"),
        _ => None,
    };
    let wait = resets_at
        .map(|at| at.saturating_mul(1000) - now_ms)
        .filter(|ms| *ms > 0 && *ms <= 30 * 24 * 60 * 60 * 1000)
        .map(|ms| {
            let minutes = (ms + 59_999) / 60_000;
            match (minutes / 60, minutes % 60) {
                (0, minutes) => format!("{minutes}m"),
                (hours, 0) => format!("{hours}h"),
                (hours, minutes) => format!("{hours}h {minutes}m"),
            }
        });
    format!(
        "Claude usage limit reached. This turn is paused until the {}limit resets{}.",
        label.map_or(String::new(), |label| format!("{label} ")),
        wait.map_or(String::new(), |wait| format!(" in {wait}"))
    )
}
pub fn delegated_notification(
    task_ids: &[NodeId],
    parent_run: &RunId,
    tasks: &[Task],
) -> Notification {
    let selected = task_ids
        .iter()
        .map(|id| tasks.iter().find(|task| &task.id == id))
        .collect::<Vec<_>>();
    let outcome = combined_outcome(
        selected
            .iter()
            .map(|task| task.map_or(NotificationOutcome::Unknown, |task| task.status.into())),
    );
    let verb = match outcome {
        NotificationOutcome::Failed => "failed",
        NotificationOutcome::Cancelled => "stopped",
        _ => "finished",
    };
    let labels = selected
        .iter()
        .filter_map(|task| {
            task.and_then(|task| {
                label(Some(
                    task.title
                        .as_deref()
                        .filter(|title| !title.trim().is_empty())
                        .unwrap_or(&task.prompt),
                ))
            })
        })
        .collect::<Vec<_>>();
    let (summary, child_thread) = if task_ids.len() == 1 {
        (
            format!(
                "Delegated task{} {verb}",
                labels
                    .first()
                    .map_or(String::new(), |label| format!(" \"{label}\""))
            ),
            selected[0].map(|task| task.child_thread.clone()),
        )
    } else {
        let total = tasks
            .iter()
            .filter(|task| task.app_owned() && task.run.as_ref() == Some(parent_run))
            .count();
        let count = if total > task_ids.len() {
            format!("{} of {total}", task_ids.len())
        } else {
            task_ids.len().to_string()
        };
        (
            format!(
                "{count} delegated tasks {verb}{}",
                if labels.is_empty() {
                    String::new()
                } else {
                    format!(": {}", labels.join(", "))
                }
            ),
            None,
        )
    };
    Notification {
        detail: None,
        source: NotificationSource::Delegated {
            task_ids: task_ids.to_vec(),
        },
        child_thread,
        outcome,
        summary,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn report(kind: BackgroundKind, label: &str, outcome: NotificationOutcome) -> WorkReport {
        WorkReport {
            kind,
            label: Some(label.into()),
            outcome,
            child_thread: None,
            exit_code: None,
        }
    }
    #[test]
    fn notification_expectations_match_the_reference() {
        assert_eq!(background_notification(&[]), None);
        let child = ThreadId::new("thread:child").unwrap();
        let mut subagent = report(
            BackgroundKind::Subagent,
            "Review src/math.ts",
            NotificationOutcome::Completed,
        );
        subagent.child_thread = Some(child.clone());
        assert_eq!(
            background_notification(&[subagent]),
            Some(Notification {
                detail: None,
                source: NotificationSource::Native(BackgroundKind::Subagent),
                child_thread: Some(child.clone()),
                outcome: NotificationOutcome::Completed,
                summary: "Subagent \"Review src/math.ts\" finished".into()
            })
        );
        let mut command = report(
            BackgroundKind::Command,
            "npm test\nnpm run lint",
            NotificationOutcome::Failed,
        );
        command.exit_code = Some(1);
        assert_eq!(
            background_notification(&[command]).unwrap().summary,
            "Command \"npm test\" failed (exit 1)"
        );
        assert_eq!(
            background_notification(&[report(
                BackgroundKind::Monitor,
                "Three ticks",
                NotificationOutcome::Updated
            )])
            .unwrap()
            .summary,
            "Monitor \"Three ticks\" reported new output"
        );
        assert_eq!(
            background_notification(&[
                report(
                    BackgroundKind::Subagent,
                    "Agent B",
                    NotificationOutcome::Cancelled
                ),
                report(
                    BackgroundKind::Command,
                    "Sleep 60 seconds",
                    NotificationOutcome::Cancelled
                )
            ])
            .unwrap(),
            Notification {
                detail: None,
                source: NotificationSource::Native(BackgroundKind::BackgroundTask),
                child_thread: None,
                outcome: NotificationOutcome::Cancelled,
                summary: "Subagent \"Agent B\" and command \"Sleep 60 seconds\" were stopped"
                    .into()
            }
        );
        assert_eq!(
            background_notification(&["a", "b", "c", "d"].map(|l| report(
                BackgroundKind::Subagent,
                l,
                NotificationOutcome::Completed
            )))
            .unwrap()
            .summary,
            "4 subagents finished"
        );
    }
}
