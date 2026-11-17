//! The work a conversation waits on after its agent answered, as T3 Code's
//! background status names it:
//! https://github.com/pingdotgg/t3code/blob/dac8b11ddc25bd5da62a367fdb6c3de777ae2986/packages/client-runtime/src/state/threadExecution.ts#L290
use crate::localization::UiLanguage;
use agent_protocol::execution::{
    BackgroundOutcome, BackgroundReport, BackgroundTask, BackgroundTaskKind,
};

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct BackgroundWork {
    /// Grouped by kind: agents first, loose tasks last.
    pub items: Vec<BackgroundWorkItem>,
    /// True when the work will wake the agent (subagents, monitors). False
    /// when only commands remain, such as a dev server: the agent is done.
    pub waiting: bool,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct BackgroundWorkItem {
    pub id: String,
    pub kind: BackgroundTaskKind,
    /// The provider's name for the work, verbatim.
    pub description: Option<String>,
}

impl BackgroundWork {
    pub fn new(tasks: &[BackgroundTask], foreground_working: bool) -> Option<Self> {
        // T3 useThreadLifecycle shows pending background tasks only while the
        // foreground is idle. A new foreground turn owns its own stop control.
        if foreground_working || tasks.is_empty() {
            return None;
        }
        let mut items: Vec<_> = tasks
            .iter()
            .map(|task| BackgroundWorkItem {
                id: task.id.clone(),
                kind: task.kind,
                description: task
                    .description
                    .as_deref()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned),
            })
            .collect();
        items.sort_by_key(|item| item.kind);
        Some(Self {
            items,
            waiting: agent_protocol::execution::background_holds_completion(tasks),
        })
    }

    /// "Waiting on subagent Review src/math.ts", "Waiting on 2 subagents and
    /// 1 command", or "Running: Start the dev server" when only commands remain.
    pub fn title(&self, language: UiLanguage) -> String {
        use UiLanguage::*;
        if let [only] = self.items.as_slice() {
            let noun = noun(only.kind, language, 1);
            return match (language, self.waiting, only.description.as_deref()) {
                (English, true, Some(label)) => format!("Waiting on {noun} {label}"),
                (English, true, None) => format!("Waiting on a {noun}"),
                (English, false, Some(label)) => format!("Running: {label}"),
                (English, false, None) => format!("Running a {noun}"),
                (Japanese, true, Some(label)) => format!("{noun}「{label}」を待機中"),
                (Japanese, true, None) => format!("{noun}を待機中"),
                (Japanese, false, Some(label)) => format!("実行中: {label}"),
                (Japanese, false, None) => format!("{noun}を実行中"),
            };
        }
        let mut groups: Vec<(BackgroundTaskKind, usize)> = Vec::new();
        for item in &self.items {
            match groups.last_mut() {
                Some((kind, count)) if *kind == item.kind => *count += 1,
                _ => groups.push((item.kind, 1)),
            }
        }
        let groups: Vec<_> = groups
            .into_iter()
            .map(|(kind, count)| match language {
                English => format!("{count} {}", noun(kind, language, count)),
                Japanese => format!("{} {count}件", noun(kind, language, count)),
            })
            .collect();
        match (language, self.waiting) {
            (English, true) => format!("Waiting on {}", join_with_and(&groups)),
            (English, false) => format!("Running {}", join_with_and(&groups)),
            (Japanese, true) => format!("{}を待機中", groups.join("、")),
            (Japanese, false) => format!("{}を実行中", groups.join("、")),
        }
    }

    /// Each item's name, or its noun when the provider gave none. A single
    /// item is already in the title.
    pub fn description(&self, language: UiLanguage) -> Option<String> {
        if self.items.len() < 2 {
            return None;
        }
        let labels: Vec<_> = self
            .items
            .iter()
            .map(|item| {
                item.description
                    .clone()
                    .unwrap_or_else(|| noun(item.kind, language, 1).into())
            })
            .collect();
        Some(labels.join(match language {
            UiLanguage::English => ", ",
            UiLanguage::Japanese => "、",
        }))
    }
}

fn noun(kind: BackgroundTaskKind, language: UiLanguage, count: usize) -> &'static str {
    use BackgroundTaskKind::{Command, Monitor, Subagent};
    const UNNAMED: BackgroundTaskKind = BackgroundTaskKind::BackgroundTask;
    match (language, kind, count == 1) {
        (UiLanguage::English, Subagent, true) => "subagent",
        (UiLanguage::English, Subagent, false) => "subagents",
        (UiLanguage::English, Command, true) => "command",
        (UiLanguage::English, Command, false) => "commands",
        (UiLanguage::English, Monitor, true) => "monitor",
        (UiLanguage::English, Monitor, false) => "monitors",
        (UiLanguage::English, UNNAMED, true) => "background task",
        (UiLanguage::English, UNNAMED, false) => "background tasks",
        (UiLanguage::Japanese, Subagent, _) => "サブエージェント",
        (UiLanguage::Japanese, Command, _) => "コマンド",
        (UiLanguage::Japanese, Monitor, _) => "モニター",
        (UiLanguage::Japanese, UNNAMED, _) => "バックグラウンドタスク",
    }
}

/// The icon of a notification row, as T3 Code's work log draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum NotificationIcon {
    Bot,
    Terminal,
    Eye,
    Zap,
    CircleAlert,
}

/// A failure shows as one; otherwise the reports' kind, and mixed kinds are
/// generic background work.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn notification_icon(reports: Vec<BackgroundReport>) -> NotificationIcon {
    if combined_outcome(&reports) == BackgroundOutcome::Failed {
        return NotificationIcon::CircleAlert;
    }
    let kind = reports
        .first()
        .map_or(BackgroundTaskKind::BackgroundTask, |first| {
            if reports.iter().all(|report| report.kind == first.kind) {
                first.kind
            } else {
                BackgroundTaskKind::BackgroundTask
            }
        });
    match kind {
        BackgroundTaskKind::Subagent => NotificationIcon::Bot,
        BackgroundTaskKind::Command => NotificationIcon::Terminal,
        BackgroundTaskKind::Monitor => NotificationIcon::Eye,
        BackgroundTaskKind::BackgroundTask => NotificationIcon::Zap,
    }
}

/// What the background work a notification reports did, as T3 Code's
/// notification summary names it: `Command "npm run dev" finished (exit 0)`.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn notification_text(reports: Vec<BackgroundReport>, language: UiLanguage) -> String {
    use UiLanguage::*;
    let outcome = combined_outcome(&reports);
    let single = reports.len() == 1;
    let verb = match (language, outcome) {
        (English, BackgroundOutcome::Failed) => "failed",
        (English, BackgroundOutcome::Cancelled) if single => "was stopped",
        (English, BackgroundOutcome::Cancelled) => "were stopped",
        (English, BackgroundOutcome::Updated)
            if single && reports[0].kind == BackgroundTaskKind::Monitor =>
        {
            "reported new output"
        }
        (English, BackgroundOutcome::Updated) => "updated",
        (English, _) => "finished",
        (Japanese, BackgroundOutcome::Failed) => "が失敗しました",
        (Japanese, BackgroundOutcome::Cancelled) => "が停止されました",
        (Japanese, BackgroundOutcome::Updated)
            if single && reports[0].kind == BackgroundTaskKind::Monitor =>
        {
            "が新しい出力を報告しました"
        }
        (Japanese, BackgroundOutcome::Updated) => "が更新されました",
        (Japanese, _) => "が完了しました",
    };
    let subject = if !single
        && (reports.len() > 3 || reports.iter().all(|report| report_label(report).is_none()))
    {
        let kind = if reports.iter().all(|report| report.kind == reports[0].kind) {
            reports[0].kind
        } else {
            BackgroundTaskKind::BackgroundTask
        };
        let noun = noun(kind, language, reports.len());
        match language {
            English => format!("{} {noun}", reports.len()),
            Japanese => format!("{noun} {}件", reports.len()),
        }
    } else {
        // One report's exit follows its verb; in a list, each name's own.
        let names: Vec<_> = reports
            .iter()
            .enumerate()
            .map(|(index, report)| named_report(report, language, index == 0, !single))
            .collect();
        match language {
            English => join_with_and(&names),
            Japanese => names.join("、"),
        }
    };
    let exit = reports
        .first()
        .filter(|_| single)
        .map(|report| exit_suffix(report, language))
        .unwrap_or_default();
    match language {
        English => format!("{subject} {verb}{exit}"),
        Japanese => format!("{subject}{verb}{exit}"),
    }
}

fn exit_suffix(report: &BackgroundReport, language: UiLanguage) -> String {
    match (report.exit_code, report.kind, language) {
        (Some(code), BackgroundTaskKind::Command, UiLanguage::English) => format!(" (exit {code})"),
        (Some(code), BackgroundTaskKind::Command, UiLanguage::Japanese) => {
            format!("（終了コード {code}）")
        }
        _ => String::new(),
    }
}

/// `Command "npm run dev" (exit 0)`, the first capitalized as T3 Code's.
fn named_report(
    report: &BackgroundReport,
    language: UiLanguage,
    first: bool,
    exit: bool,
) -> String {
    let noun = noun(report.kind, language, 1);
    let label = report_label(report);
    let named = match language {
        UiLanguage::English => {
            let mut named = if first {
                let mut chars = noun.chars();
                chars
                    .next()
                    .map(|initial| initial.to_uppercase().chain(chars).collect())
                    .unwrap_or_default()
            } else {
                noun.to_owned()
            };
            if let Some(label) = label {
                named.push_str(&format!(" \"{label}\""));
            }
            named
        }
        UiLanguage::Japanese => match label {
            Some(label) => format!("{noun}「{label}」"),
            None => noun.to_owned(),
        },
    };
    if exit {
        named + &exit_suffix(report, language)
    } else {
        named
    }
}

/// A report's name on one line, shortened as T3 Code's.
fn report_label(report: &BackgroundReport) -> Option<String> {
    const MAX: usize = 80;
    let line = report.label.as_deref()?.trim().lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    Some(if line.chars().count() > MAX {
        line.chars().take(MAX - 1).chain(['…']).collect()
    } else {
        line.to_owned()
    })
}

fn combined_outcome(reports: &[BackgroundReport]) -> BackgroundOutcome {
    let has = |outcome| reports.iter().any(|report| report.outcome == outcome);
    if has(BackgroundOutcome::Failed) {
        BackgroundOutcome::Failed
    } else if has(BackgroundOutcome::Cancelled) {
        BackgroundOutcome::Cancelled
    } else if !reports.is_empty()
        && reports
            .iter()
            .all(|report| report.outcome == BackgroundOutcome::Completed)
    {
        BackgroundOutcome::Completed
    } else if has(BackgroundOutcome::Updated) {
        BackgroundOutcome::Updated
    } else {
        BackgroundOutcome::Unknown
    }
}

fn join_with_and(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [only] => only.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use BackgroundTaskKind::{Command, Monitor, Subagent};
    use UiLanguage::*;

    fn work(tasks: &[(BackgroundTaskKind, Option<&str>)]) -> BackgroundWork {
        BackgroundWork::new(
            &tasks
                .iter()
                .enumerate()
                .map(|(index, (kind, description))| BackgroundTask {
                    id: index.to_string(),
                    kind: *kind,
                    description: description.map(str::to_owned),
                })
                .collect::<Vec<_>>(),
            false,
        )
        .unwrap()
    }

    #[test]
    fn notifications_name_what_the_work_did_in_either_language_as_t3_code() {
        use BackgroundOutcome::*;
        let report = |kind, label: Option<&str>, outcome| BackgroundReport {
            kind,
            label: label.map(str::to_owned),
            outcome,
            exit_code: None,
        };
        let exited = BackgroundReport {
            exit_code: Some(0),
            ..report(Command, Some("npm run dev\nmore"), Completed)
        };
        for (reports, english, japanese, icon) in [
            (
                vec![exited],
                "Command \"npm run dev\" finished (exit 0)",
                "コマンド「npm run dev」が完了しました（終了コード 0）",
                NotificationIcon::Terminal,
            ),
            (
                vec![report(Monitor, Some("Watch the build"), Updated)],
                "Monitor \"Watch the build\" reported new output",
                "モニター「Watch the build」が新しい出力を報告しました",
                NotificationIcon::Eye,
            ),
            (
                vec![report(Subagent, None, Cancelled)],
                "Subagent was stopped",
                "サブエージェントが停止されました",
                NotificationIcon::Bot,
            ),
            (
                vec![
                    report(Subagent, Some("Review"), Completed),
                    report(Command, Some("npm test"), Failed),
                ],
                "Subagent \"Review\" and command \"npm test\" failed",
                "サブエージェント「Review」、コマンド「npm test」が失敗しました",
                NotificationIcon::CircleAlert,
            ),
            (
                vec![
                    report(Command, None, Completed),
                    report(Command, None, Unknown),
                ],
                "2 commands finished",
                "コマンド 2件が完了しました",
                NotificationIcon::Terminal,
            ),
            (
                vec![
                    report(Command, Some("a"), Completed),
                    report(Monitor, Some("b"), Completed),
                    report(Subagent, Some("c"), Completed),
                    report(Command, Some("d"), Cancelled),
                ],
                "4 background tasks were stopped",
                "バックグラウンドタスク 4件が停止されました",
                NotificationIcon::Zap,
            ),
        ] {
            assert_eq!(notification_text(reports.clone(), English), english);
            assert_eq!(notification_text(reports.clone(), Japanese), japanese);
            assert_eq!(notification_icon(reports), icon);
        }
    }

    #[test]
    fn nothing_running_presents_no_work() {
        assert_eq!(BackgroundWork::new(&[], false), None);
    }

    #[test]
    fn background_status_does_not_replace_a_foreground_turns_status() {
        let tasks = [BackgroundTask {
            id: "server".into(),
            kind: Command,
            description: Some("npm run dev".into()),
        }];
        assert!(BackgroundWork::new(&tasks, false).is_some());
        assert_eq!(BackgroundWork::new(&tasks, true), None);
    }

    #[test]
    fn a_single_item_names_itself_in_the_title_as_t3_code() {
        for (task, english, japanese) in [
            (
                (Subagent, Some("Review src/math.ts")),
                "Waiting on subagent Review src/math.ts",
                "サブエージェント「Review src/math.ts」を待機中",
            ),
            ((Monitor, None), "Waiting on a monitor", "モニターを待機中"),
            (
                (Command, Some(" Start the dev server ")),
                "Running: Start the dev server",
                "実行中: Start the dev server",
            ),
            (
                (Command, Some("  ")),
                "Running a command",
                "コマンドを実行中",
            ),
            (
                (BackgroundTaskKind::BackgroundTask, None),
                "Waiting on a background task",
                "バックグラウンドタスクを待機中",
            ),
        ] {
            let work = work(&[task]);
            assert_eq!(work.title(English), english);
            assert_eq!(work.title(Japanese), japanese);
            assert_eq!(work.description(English), None);
        }
    }

    #[test]
    fn several_items_group_by_kind_with_agents_first_and_list_their_names() {
        let work = work(&[
            (Command, Some("npm run dev")),
            (Monitor, None),
            (Subagent, Some("Explore")),
            (Subagent, Some("Review")),
        ]);
        assert!(work.waiting, "subagents and monitors wake the agent");
        assert_eq!(
            work.title(English),
            "Waiting on 2 subagents, 1 command and 1 monitor"
        );
        assert_eq!(
            work.title(Japanese),
            "サブエージェント 2件、コマンド 1件、モニター 1件を待機中"
        );
        assert_eq!(
            work.description(English).as_deref(),
            Some("Explore, Review, npm run dev, monitor")
        );
        assert_eq!(
            work.description(Japanese).as_deref(),
            Some("Explore、Review、npm run dev、モニター")
        );
        let commands = super::BackgroundWork::new(
            &[
                BackgroundTask {
                    id: "a".into(),
                    kind: Command,
                    description: None,
                },
                BackgroundTask {
                    id: "b".into(),
                    kind: Command,
                    description: None,
                },
            ],
            false,
        )
        .unwrap();
        assert!(
            !commands.waiting,
            "a dev server does not keep the agent working"
        );
        assert_eq!(commands.title(English), "Running 2 commands");
        assert_eq!(commands.title(Japanese), "コマンド 2件を実行中");
    }
}
