//! A thread's terminals: the drawer tabs and one terminal's output.
use crate::state::{Snapshot, Terminal, TerminalOutput, TerminalPhase};
use agent_domain::ThreadId;
use agent_protocol::operations::thread_terminal_handle_for;

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

/// The id a new terminal of the thread takes.
pub fn next_terminal_id(snapshot: &Snapshot, thread: &ThreadId) -> String {
    let last = thread_terminals(snapshot, thread)
        .filter_map(|terminal| ordinal(&terminal.terminal_id))
        .max()
        .unwrap_or(0);
    format!("{TERMINAL_PREFIX}{}", last + 1)
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

/// The drawer's tabs: setup terminals first, then terminals in creation order.
pub fn terminal_tabs(snapshot: &Snapshot, thread: &ThreadId) -> Vec<TerminalTab> {
    let mut terminals: Vec<&Terminal> = thread_terminals(snapshot, thread).collect();
    terminals.sort_by_key(|terminal| {
        (
            ordinal(&terminal.terminal_id).is_some(),
            ordinal(&terminal.terminal_id),
            terminal.terminal_id.clone(),
        )
    });
    terminals
        .into_iter()
        .map(|terminal| TerminalTab {
            terminal_id: terminal.terminal_id.clone(),
            group: terminal.group.clone(),
            label: match terminal.terminal_id.strip_prefix(SETUP_TERMINAL_PREFIX) {
                Some(script) => setup_label(snapshot, thread, script),
                None => ordinal(&terminal.terminal_id)
                    .map_or_else(|| terminal.terminal_id.clone(), |n| format!("Terminal {n}")),
            },
            status: status(&terminal.phase),
            running: matches!(
                terminal.phase,
                TerminalPhase::Starting | TerminalPhase::Running
            ),
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::operations::TerminalSize;

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
    fn lists_setup_terminals_first_and_numbers_the_next_terminal() {
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
        assert_eq!(next_terminal_id(&snapshot, &thread), "term-11");
        assert_eq!(
            next_terminal_id(&snapshot, &ThreadId::new("new").unwrap()),
            "term-1"
        );
        let view = terminal_view(&snapshot, &thread, "term-2", 0);
        assert_eq!(view.status.as_deref(), Some("Running"));
        assert!(!view.accepts_input);
    }
}
