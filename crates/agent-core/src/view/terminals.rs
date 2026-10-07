//! A thread's terminals: the drawer tabs and one terminal's output.
use crate::state::{Snapshot, Terminal, TerminalOutput, TerminalPhase};
use agent_domain::ThreadId;
use agent_protocol::operations::thread_terminal_handle_for;

pub mod output_context;
pub mod text_size;

pub const SETUP_TERMINAL_PREFIX: &str = "setup-";
const TERMINAL_PREFIX: &str = "term-";

/// The terminal a project setup script runs in.
pub fn setup_terminal_id(script_id: &str) -> String {
    format!("{SETUP_TERMINAL_PREFIX}{script_id}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TerminalTab {
    pub terminal_id: String,
    /// Tabs with the same group are split side by side.
    pub group: String,
    pub label: String,
    pub status: String,
    pub running: bool,
    /// The shell runs a command.
    pub running_process: bool,
    /// The shell ended or failed to start.
    pub exited: bool,
    /// Where the shell runs.
    pub cwd: String,
    /// The mobile terminal menu's status: "Task running", "Ready",
    /// "Starting", "Exited", "Error" or "Not started".
    pub menu_status: String,
}

/// The mobile terminal menu's status of a terminal.
pub fn terminal_menu_status(phase: &TerminalPhase, running_process: bool) -> String {
    match phase {
        TerminalPhase::Running if running_process => "Task running",
        TerminalPhase::Running => "Ready",
        TerminalPhase::Starting => "Starting",
        TerminalPhase::Exited(_) => "Exited",
        TerminalPhase::Failed(_) => "Error",
        TerminalPhase::Suspended | TerminalPhase::Detached => "Not started",
    }
    .into()
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TerminalView {
    pub status: Option<String>,
    pub loading: bool,
    pub accepts_input: bool,
    pub output: Vec<TerminalOutput>,
}

fn status(phase: &TerminalPhase) -> String {
    match phase {
        TerminalPhase::Starting => "Starting".into(),
        TerminalPhase::Running => "Running".into(),
        TerminalPhase::Suspended => "Waiting for reconnect".into(),
        TerminalPhase::Detached => "Detached".into(),
        TerminalPhase::Exited(code) => format!("Exited · {code}"),
        TerminalPhase::Failed(message) => message.clone(),
    }
}

fn ordinal(terminal_id: &str) -> Option<u32> {
    terminal_id.strip_prefix(TERMINAL_PREFIX)?.parse().ok()
}

/// The lowest `term-N` no terminal of the thread uses.
pub fn next_terminal_id(snapshot: &Snapshot, thread: &ThreadId) -> String {
    let used: std::collections::BTreeSet<String> = thread_terminals(snapshot, thread)
        .map(|terminal| terminal.terminal_id.clone())
        .chain(
            snapshot
                .terminal_metadata
                .keys()
                .filter(|(owner, _)| owner == thread)
                .map(|(_, id)| id.clone()),
        )
        .collect();
    (1..)
        .map(|index| format!("{TERMINAL_PREFIX}{index}"))
        .find(|id| !used.contains(id))
        .expect("an unused terminal id")
}

/// The thread's terminals whose shell runs a command, by id.
pub fn running_terminal_ids(snapshot: &Snapshot, thread: &ThreadId) -> Vec<String> {
    snapshot
        .terminal_metadata
        .values()
        .filter(|summary| &summary.thread == thread && summary.has_running_subprocess)
        .map(|summary| summary.terminal_id.clone())
        .collect()
}

/// "1 terminal process running", "2 terminal processes running".
pub fn terminal_process_label(count: usize) -> String {
    format!(
        "{count} terminal {} running",
        if count == 1 { "process" } else { "processes" }
    )
}

fn thread_terminals<'a>(
    snapshot: &'a Snapshot,
    thread: &'a ThreadId,
) -> impl Iterator<Item = &'a Terminal> {
    snapshot
        .terminals
        .values()
        .filter(move |terminal| &terminal.thread == thread)
}

fn setup_label(snapshot: &Snapshot, thread: &ThreadId, script_id: &str) -> String {
    let project = snapshot
        .thread_state(thread)
        .and_then(|state| state.thread.as_ref())
        .map(|thread| &thread.project)
        .or_else(|| snapshot.thread_row(thread).map(|row| &row.project));
    snapshot
        .shell_projects()
        .iter()
        .filter(|candidate| Some(&candidate.id) == project)
        .flat_map(|project| &project.scripts)
        .find(|script| script.id == script_id)
        .map_or_else(|| "Setup".into(), |script| script.name.clone())
}

/// The terminal's name: its setup script, the Host's title or "Terminal N".
pub fn tab_label(snapshot: &Snapshot, thread: &ThreadId, terminal_id: &str) -> String {
    if let Some(script) = terminal_id.strip_prefix(SETUP_TERMINAL_PREFIX) {
        return setup_label(snapshot, thread, script);
    }
    snapshot
        .terminal_metadata
        .get(&(thread.clone(), terminal_id.to_owned()))
        .map(|summary| summary.label.trim())
        .filter(|label| !label.is_empty())
        .map_or_else(
            || agent_protocol::operations::terminal_label(terminal_id),
            str::to_owned,
        )
}

/// The drawer's tabs: setup terminals first, then terminals in creation
/// order. Live terminals the Host reports that this device has not opened
/// are listed too, so it can attach to them.
pub fn terminal_tabs(snapshot: &Snapshot, thread: &ThreadId) -> Vec<TerminalTab> {
    use agent_protocol::operations::TerminalStatus;
    let summary = |terminal_id: &str| {
        snapshot
            .terminal_metadata
            .get(&(thread.clone(), terminal_id.to_owned()))
    };
    let mut tabs: Vec<TerminalTab> = thread_terminals(snapshot, thread)
        .map(|terminal| TerminalTab {
            terminal_id: terminal.terminal_id.clone(),
            group: terminal.group.clone(),
            label: tab_label(snapshot, thread, &terminal.terminal_id),
            status: status(&terminal.phase),
            running: matches!(
                terminal.phase,
                TerminalPhase::Starting | TerminalPhase::Running
            ),
            running_process: summary(&terminal.terminal_id)
                .is_some_and(|summary| summary.has_running_subprocess),
            exited: matches!(
                terminal.phase,
                TerminalPhase::Exited(_) | TerminalPhase::Failed(_)
            ),
            cwd: summary(&terminal.terminal_id)
                .map_or_else(|| terminal.cwd.clone(), |summary| summary.cwd.clone()),
            menu_status: terminal_menu_status(
                &terminal.phase,
                summary(&terminal.terminal_id)
                    .is_some_and(|summary| summary.has_running_subprocess),
            ),
        })
        .collect();
    let remote: Vec<TerminalTab> = snapshot
        .terminal_metadata
        .values()
        .filter(|summary| {
            &summary.thread == thread
                && matches!(
                    summary.status,
                    TerminalStatus::Running | TerminalStatus::Starting
                )
                && !tabs
                    .iter()
                    .any(|tab| tab.terminal_id == summary.terminal_id)
        })
        .map(|summary| TerminalTab {
            terminal_id: summary.terminal_id.clone(),
            group: summary.terminal_id.clone(),
            label: tab_label(snapshot, thread, &summary.terminal_id),
            status: match summary.status {
                TerminalStatus::Starting => "Starting",
                _ => "Running",
            }
            .into(),
            running: true,
            running_process: summary.has_running_subprocess,
            exited: false,
            cwd: summary.cwd.clone(),
            menu_status: terminal_menu_status(
                &match summary.status {
                    TerminalStatus::Starting => TerminalPhase::Starting,
                    _ => TerminalPhase::Running,
                },
                summary.has_running_subprocess,
            ),
        })
        .collect();
    tabs.extend(remote);
    tabs.sort_by_key(|tab| {
        (
            ordinal(&tab.terminal_id).is_some(),
            ordinal(&tab.terminal_id),
            tab.terminal_id.clone(),
        )
    });
    tabs
}

/// One terminal's output after `after`.
pub fn terminal_view(
    snapshot: &Snapshot,
    thread: &ThreadId,
    terminal_id: &str,
    after: u64,
) -> TerminalView {
    let terminal = snapshot
        .terminals
        .get(&thread_terminal_handle_for(thread.as_str(), terminal_id));
    TerminalView {
        status: terminal.map(|t| status(&t.phase)),
        loading: terminal.is_some_and(|t| t.phase == TerminalPhase::Starting),
        accepts_input: snapshot.connected
            && terminal.is_some_and(|t| t.phase == TerminalPhase::Running),
        output: terminal
            .map(|t| {
                t.output
                    .iter()
                    .filter(|o| o.sequence > after)
                    .map(|output| output.as_ref().clone())
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// The most text, in UTF-16 units, one terminal context record carries.
pub const TERMINAL_CONTEXT_TEXT_MAX_CHARS: usize = 64_000;

/// A terminal's visible output as the attach sheet lists it: trailing
/// newlines dropped, one entry per line.
pub fn visible_output_lines(text: &str) -> Vec<String> {
    text.trim_end_matches('\n')
        .split('\n')
        .map(str::to_owned)
        .collect()
}

/// The lines chosen in the attach sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct VisibleOutputSelection {
    pub text: String,
    /// Over one record's limit: the sheet asks for fewer lines.
    pub too_large: bool,
    pub can_attach: bool,
}

/// Lines `start` through `end` (0-based, inclusive) of the visible output.
pub fn visible_output_selection(lines: &[String], start: u32, end: u32) -> VisibleOutputSelection {
    let (start, end) = (start as usize, end as usize);
    let text = lines
        .get(start..=end.min(lines.len().saturating_sub(1)))
        .unwrap_or_default()
        .join("\n");
    let too_large = crate::js_text::utf16_len(&text) > TERMINAL_CONTEXT_TEXT_MAX_CHARS;
    VisibleOutputSelection {
        can_attach: !crate::js_text::js_trim(&text).is_empty() && !too_large,
        text,
        too_large,
    }
}

/// Visible terminal lines attached to the composer as context.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TerminalOutputContext {
    pub terminal_id: String,
    pub terminal_label: String,
    /// 1-based, relative to the frozen viewport rather than the scrollback.
    pub line_start: u32,
    pub line_end: u32,
    pub text: String,
}

impl TerminalOutputContext {
    /// The message context record and the composer link to it.
    pub fn record(&self, context_id: &str) -> (serde_json::Value, String) {
        let label = format!(
            "{} · visible lines {}–{}",
            self.terminal_label, self.line_start, self.line_end
        );
        let reference =
            crate::view::composer::chips::format_context_reference("terminal", context_id, &label);
        let record = serde_json::json!({
            "version": 1,
            "kind": "terminal",
            "contextId": context_id,
            "label": label,
            "terminalId": self.terminal_id,
            "terminalLabel": format!("{} (visible output)", self.terminal_label),
            "lineStart": self.line_start,
            "lineEnd": self.line_end,
            "text": self.text,
        });
        (record, reference)
    }
}

/// `text` with `reference` added at its end, spaced from the text before it
/// and followed by a space for typing on.
pub fn append_context_reference(text: &str, reference: &str) -> String {
    let lead = if !text.is_empty()
        && !text.ends_with(crate::js_text::is_js_space)
        && !reference.starts_with(crate::js_text::is_js_space)
    {
        " "
    } else {
        ""
    };
    let trail = if reference.ends_with(crate::js_text::is_js_space) {
        ""
    } else {
        " "
    };
    format!("{text}{lead}{reference}{trail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::operations::TerminalSize;

    fn lines(text: &str) -> Vec<String> {
        visible_output_lines(text)
    }

    #[test]
    fn the_attach_sheet_lists_visible_lines_without_trailing_newlines() {
        assert_eq!(lines("a\n\nb\n\n\n"), ["a", "", "b"]);
        assert_eq!(lines(""), [""]);
    }

    #[test]
    fn a_selection_attaches_its_lines_unless_blank_or_over_the_limit() {
        let output = lines("$ cargo test\nok\n   \n");
        let chosen = visible_output_selection(&output, 0, 1);
        assert_eq!(chosen.text, "$ cargo test\nok");
        assert!(chosen.can_attach && !chosen.too_large);
        assert!(!visible_output_selection(&output, 2, 2).can_attach);
        let huge = vec!["x".repeat(TERMINAL_CONTEXT_TEXT_MAX_CHARS + 1)];
        let over = visible_output_selection(&huge, 0, 0);
        assert!(over.too_large && !over.can_attach);
    }

    #[test]
    fn attached_output_names_its_terminal_and_visible_line_range() {
        let context = TerminalOutputContext {
            terminal_id: "term-1".into(),
            terminal_label: "Terminal 1".into(),
            line_start: 3,
            line_end: 7,
            text: "error: boom".into(),
        };
        let (record, reference) = context.record("ctx-1");
        assert_eq!(
            reference,
            "[Terminal 1 · visible lines 3–7](context://v1/terminal/ctx-1)"
        );
        assert_eq!(record["terminalLabel"], "Terminal 1 (visible output)");
        assert_eq!(record["label"], "Terminal 1 · visible lines 3–7");
        assert_eq!(
            (record["lineStart"].as_u64(), record["lineEnd"].as_u64()),
            (Some(3), Some(7))
        );
        let context = agent_domain::MessageContext {
            version: 1,
            records: vec![agent_domain::Json(record)],
        };
        assert_eq!(
            context.normalized().map(|valid| valid.records.len()),
            Some(1)
        );
    }

    #[test]
    fn appended_references_are_spaced_from_the_draft() {
        assert_eq!(append_context_reference("", "[t](x)"), "[t](x) ");
        assert_eq!(append_context_reference("look", "[t](x)"), "look [t](x) ");
        assert_eq!(append_context_reference("look ", "[t](x)"), "look [t](x) ");
    }

    fn terminal(thread: &ThreadId, id: &str, phase: TerminalPhase) -> (String, Terminal) {
        (
            thread_terminal_handle_for(thread.as_str(), id),
            Terminal {
                thread: thread.clone(),
                terminal_id: id.into(),
                group: id.into(),
                cwd: "/repo".into(),
                size: TerminalSize { cols: 80, rows: 24 },
                phase,
                output: Default::default(),
                sequence: 0,
                output_bytes: 0,
                pending_input: vec![],
            },
        )
    }

    #[test]
    fn lists_setup_terminals_first_and_takes_the_lowest_free_id() {
        let thread = ThreadId::new("thread").unwrap();
        let other = ThreadId::new("other").unwrap();
        let snapshot = Snapshot {
            terminals: [
                terminal(&thread, "term-2", TerminalPhase::Running),
                terminal(&thread, "term-10", TerminalPhase::Exited(0)),
                terminal(&thread, "setup-install", TerminalPhase::Running),
                terminal(&other, "term-30", TerminalPhase::Running),
            ]
            .into_iter()
            .collect(),
            ..Snapshot::default()
        };
        let tabs = terminal_tabs(&snapshot, &thread);
        assert_eq!(
            tabs.iter()
                .map(|tab| (tab.terminal_id.as_str(), tab.label.as_str(), tab.running))
                .collect::<Vec<_>>(),
            [
                ("setup-install", "Setup", true),
                ("term-2", "Terminal 2", true),
                ("term-10", "Terminal 10", false),
            ]
        );
        assert_eq!(next_terminal_id(&snapshot, &thread), "term-1");
        assert_eq!(
            next_terminal_id(&snapshot, &ThreadId::new("new").unwrap()),
            "term-1"
        );
        let view = terminal_view(&snapshot, &thread, "term-2", 0);
        assert_eq!(view.status.as_deref(), Some("Running"));
        assert!(!view.accepts_input);
    }

    fn summary(
        thread: &ThreadId,
        id: &str,
        running: bool,
    ) -> agent_protocol::operations::TerminalSummary {
        agent_protocol::operations::TerminalSummary {
            thread: thread.clone(),
            terminal_id: id.into(),
            cwd: "/repo".into(),
            worktree_path: None,
            status: agent_protocol::operations::TerminalStatus::Running,
            pid: Some(1),
            exit_code: None,
            has_running_subprocess: running,
            label: if running {
                "vite".into()
            } else {
                String::new()
            },
            updated_at: agent_domain::Timestamp::from_millis(0).unwrap(),
        }
    }

    // terminalLabels.ts nextTerminalId and resolveTerminalSessionLabel, and the
    // known sessions the terminal menu lists.
    #[test]
    fn host_terminals_name_their_command_and_new_ids_fill_the_lowest_gap() {
        let thread = ThreadId::new("thread").unwrap();
        let mut snapshot = Snapshot::default();
        snapshot
            .terminals
            .extend([terminal(&thread, "term-2", TerminalPhase::Running)]);
        for (id, running) in [("term-1", true), ("term-3", false)] {
            snapshot
                .terminal_metadata
                .insert((thread.clone(), id.into()), summary(&thread, id, running));
        }
        let tabs = terminal_tabs(&snapshot, &thread);
        let labels: Vec<(&str, &str, bool)> = tabs
            .iter()
            .map(|tab| {
                (
                    tab.terminal_id.as_str(),
                    tab.label.as_str(),
                    tab.running_process,
                )
            })
            .collect();
        assert_eq!(
            labels,
            [
                ("term-1", "vite", true),
                ("term-2", "Terminal 2", false),
                ("term-3", "Terminal 3", false)
            ]
        );
        assert_eq!(next_terminal_id(&snapshot, &thread), "term-4");
        snapshot.terminal_metadata.clear();
        assert_eq!(next_terminal_id(&snapshot, &thread), "term-1");
        assert_eq!(terminal_process_label(1), "1 terminal process running");
        assert_eq!(terminal_process_label(2), "2 terminal processes running");
    }
}
