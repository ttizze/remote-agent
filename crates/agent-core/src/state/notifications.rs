//! Only connection-wide settings and terminals use notifications. Saved
//! conversation and list state arrive through their ordered subscriptions.
use super::*;

pub(super) fn notification(
    previous: &Snapshot,
    message: crate::protocol::Notification,
) -> (Snapshot, Vec<Effect>) {
    use crate::protocol::Notification;
    match message {
        Notification::ProviderSettings { settings } => {
            let mut next = previous.clone();
            let effects = if next.accept_provider_settings(settings) {
                op::refresh_models()
            } else {
                Vec::new()
            };
            (next, effects)
        }
        Notification::TerminalFailed { handle, reason } => {
            reduce(previous, "", Event::TerminalFailed { handle, reason })
        }
        event @ (Notification::Output { .. }
        | Notification::Exited { .. }
        | Notification::TerminalRestored { .. }
        | Notification::TerminalDetached { .. }) => process(previous, event),
        _ => (previous.clone(), Vec::new()),
    }
}

fn process(previous: &Snapshot, event: crate::protocol::Notification) -> (Snapshot, Vec<Effect>) {
    use crate::protocol::Notification;
    let handle = match &event {
        Notification::Output { handle, .. }
        | Notification::Exited { handle, .. }
        | Notification::TerminalRestored { handle, .. }
        | Notification::TerminalDetached { handle } => handle,
        _ => unreachable!(),
    };
    let Some(current) = previous.terminals.get(handle) else {
        return (previous.clone(), Vec::new());
    };
    if matches!(current.phase, TerminalPhase::Exited(_)) {
        return (previous.clone(), Vec::new());
    }
    let mut next = previous.clone();
    let terminal = shared_mut(&mut next.terminals, handle).unwrap();
    match event {
        Notification::Exited { code, .. } => terminal.phase = TerminalPhase::Exited(code),
        Notification::TerminalDetached { .. } => terminal.phase = TerminalPhase::Detached,
        Notification::Output { data, .. } => {
            terminal.sequence += 1;
            terminal.output.push_back(Arc::new(TerminalOutput {
                sequence: terminal.sequence,
                data,
                reset_size: None,
            }));
        }
        Notification::TerminalRestored {
            data, cols, rows, ..
        } => {
            terminal.output.clear();
            terminal.phase = TerminalPhase::Running;
            terminal.size = agent_protocol::operations::TerminalSize { cols, rows };
            terminal.sequence += 1;
            terminal.output.push_back(Arc::new(TerminalOutput {
                sequence: terminal.sequence,
                data,
                reset_size: Some(terminal.size),
            }));
        }
        _ => unreachable!(),
    }
    (next, Vec::new())
}
