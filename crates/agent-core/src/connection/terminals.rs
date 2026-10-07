//! Thread-owned terminals: one Host process per (thread, terminal id) that any
//! paired device attaches to.
use super::{
    intents::{Next, invalid},
    owner::Owner,
};
use crate::{
    peer::PeerError,
    protocol::Call,
    state::{Terminal, TerminalPhase},
    view::terminals::next_terminal_id,
};
use agent_domain::ThreadId;
use agent_protocol::operations::{self as op, thread_terminal_handle_for};

fn handle(thread: &ThreadId, terminal_id: &str) -> String {
    thread_terminal_handle_for(thread.as_str(), terminal_id)
}

impl Owner {
    /// Starts the terminal on the Host, or attaches to the running one.
    pub(super) fn open_terminal(
        &mut self,
        thread: ThreadId,
        terminal_id: String,
        group: Option<String>,
        size: op::TerminalSize,
        input: Vec<u8>,
    ) -> Result<Next, PeerError> {
        let handle = handle(&thread, &terminal_id);
        let location = self.state.terminal_location(&thread, &terminal_id);
        let cwd = location.cwd.clone();
        if cwd.is_empty() {
            return Err(invalid("The thread has no folder to open a terminal in"));
        }
        let previous = self.state.terminals.get(&handle);
        let terminal = Terminal {
            group: group
                .or_else(|| previous.map(|t| t.group.clone()))
                .unwrap_or_else(|| terminal_id.clone()),
            thread,
            terminal_id: terminal_id.clone(),
            cwd: cwd.clone(),
            size,
            phase: TerminalPhase::Starting,
            output: previous.map(|t| t.output.clone()).unwrap_or_default(),
            sequence: previous.map_or(0, |t| t.sequence),
            output_bytes: previous.map_or(0, |t| t.output_bytes),
            pending_input: input,
        };
        let restart_if_not_running = !terminal.pending_input.is_empty();
        self.state.terminals.insert(handle, terminal);
        Ok(Next::call(
            Call::StartTerminal(op::StartTerminal {
                thread: location.thread,
                terminal_id,
                cwd: Some(cwd),
                worktree_path: location.worktree_path,
                size,
                env: location.env,
                restart_if_not_running,
            }),
            None,
        ))
    }

    pub(super) fn new_terminal(
        &mut self,
        thread: ThreadId,
        split_from: Option<&str>,
        size: op::TerminalSize,
        input: Vec<u8>,
    ) -> Result<Next, PeerError> {
        let group = split_from
            .and_then(|source| self.state.terminals.get(&handle(&thread, source)))
            .map(|terminal| terminal.group.clone());
        let id = next_terminal_id(&self.state, &thread);
        self.open_terminal(thread, id, group, size, input)
    }

    /// Runs a project script in a new terminal and remembers it for the
    /// project's run button.
    pub(super) fn run_project_script(
        &mut self,
        thread: ThreadId,
        script_id: &str,
        size: op::TerminalSize,
    ) -> Result<Next, PeerError> {
        let project = self
            .state
            .thread_project(&thread)
            .ok_or_else(|| invalid("Thread is loading"))?
            .to_owned();
        let script = self
            .state
            .shell_projects()
            .iter()
            .filter(|candidate| candidate.id == project)
            .flat_map(|project| &project.scripts)
            .find(|script| script.id == script_id)
            .cloned()
            .ok_or_else(|| invalid("That script no longer exists"))?;
        self.state
            .preferences
            .last_run_scripts
            .insert(project, script.id.clone());
        self.new_terminal(
            thread,
            None,
            size,
            format!("{}\r", script.command).into_bytes(),
        )
    }

    pub(super) fn terminal_call(
        &mut self,
        thread: &ThreadId,
        terminal_id: &str,
        call: impl FnOnce(String) -> Call,
    ) -> Next {
        Next::call(call(handle(thread, terminal_id)), None)
    }

    pub(super) fn close_terminal(&mut self, thread: &ThreadId, terminal_id: &str) -> Next {
        let handle = handle(thread, terminal_id);
        self.state.terminals.remove(&handle);
        Next::call(
            Call::KillTerminal(op::TerminalKill {
                process_handle: handle,
                delete_history: true,
            }),
            None,
        )
    }

    /// Empties the terminal's history and every attached screen.
    pub(super) fn clear_terminal(&mut self, thread: ThreadId, terminal_id: String) -> Next {
        if let Some(terminal) = self.state.terminals.get_mut(&handle(&thread, &terminal_id)) {
            terminal.clear_output();
        }
        Next::call(
            Call::ClearTerminal(op::ClearTerminal {
                thread,
                terminal_id,
            }),
            None,
        )
    }

    /// Starts the terminal's shell again with an empty history.
    pub(super) fn restart_terminal(
        &mut self,
        thread: ThreadId,
        terminal_id: String,
        size: op::TerminalSize,
    ) -> Result<Next, PeerError> {
        let location = self.state.terminal_location(&thread, &terminal_id);
        if location.cwd.is_empty() {
            return Err(invalid("The thread has no folder to open a terminal in"));
        }
        let handle = handle(&thread, &terminal_id);
        let group = self
            .state
            .terminals
            .get(&handle)
            .map_or_else(|| terminal_id.clone(), |terminal| terminal.group.clone());
        self.state.terminals.insert(
            handle,
            Terminal {
                thread: thread.clone(),
                terminal_id: terminal_id.clone(),
                group,
                cwd: location.cwd.clone(),
                size,
                phase: TerminalPhase::Starting,
                output: Default::default(),
                sequence: 0,
                output_bytes: 0,
                pending_input: vec![],
            },
        );
        Ok(Next::call(
            Call::RestartTerminal(op::RestartTerminal {
                thread,
                terminal_id,
                cwd: location.cwd,
                worktree_path: location.worktree_path,
                size,
                env: location.env,
            }),
            None,
        ))
    }

    /// The Host closed the terminal; its tab goes away.
    pub(super) fn terminal_closed(&mut self, handle: &str) {
        self.state.terminals.remove(handle);
    }

    /// Sends what waited for the terminal to start.
    pub(super) fn terminal_started(&mut self, handle: &str) {
        let Some(terminal) = self.state.terminals.get_mut(handle) else {
            return;
        };
        if terminal.phase == TerminalPhase::Starting {
            terminal.phase = TerminalPhase::Running;
        }
        let input = std::mem::take(&mut terminal.pending_input);
        if !input.is_empty() {
            self.job(
                Call::WriteTerminal(op::TerminalWrite {
                    process_handle: handle.into(),
                    data: input,
                }),
                None,
                None,
            );
        }
    }
}
