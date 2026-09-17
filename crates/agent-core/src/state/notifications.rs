//! Protocol notifications mutate the same immutable snapshot as RPC responses.
use super::*;
pub(super) fn notification(
    previous: &Snapshot,
    message: crate::protocol::Notification,
) -> (Snapshot, Vec<Effect>) {
    use crate::protocol::Notification;
    if let Notification::Activity {
        session,
        active,
        finished,
    } = message
    {
        let mut next = previous.clone();
        let id = session.thread_id();
        let activity = Arc::make_mut(&mut next.activity);
        activity.active.insert(id.clone(), active);
        if active {
            activity.unread.remove(&id);
        } else if finished && previous.navigation.thread_id.as_ref() != Some(&id) {
            activity.unread.insert(id);
        }
        return (
            next,
            if active {
                Vec::new()
            } else {
                refresh_list(previous)
            },
        );
    }

    match message {
        Notification::TerminalFailed { handle, reason } => {
            reduce(previous, Event::TerminalFailed { handle, reason })
        }
        event @ (Notification::Output { .. }
        | Notification::Exited { .. }
        | Notification::TerminalRestored { .. }
        | Notification::TerminalDetached { .. }) => process(previous, event),
        Notification::Provider { method, .. } if method == "thread/name/updated" => {
            (previous.clone(), refresh_list(previous))
        }
        _ => (previous.clone(), Vec::new()),
    }
}

fn refresh_list(snapshot: &Snapshot) -> Vec<Effect> {
    if snapshot.connected {
        vec![Effect::execute(op::ListThreads::new(
            (*snapshot.list_query).clone(),
        ))]
    } else {
        Vec::new()
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
            terminal.size = crate::client::TerminalSize { cols, rows };
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

pub(super) fn session_update(
    previous: &Snapshot,
    update: crate::session::SessionUpdate,
) -> (Snapshot, Vec<Effect>) {
    let Some((id, _)) = previous
        .subscriptions
        .iter()
        .find(|(_, subscription)| **subscription == update.subscription_id)
    else {
        return (previous.clone(), Vec::new());
    };
    let mut next = previous.clone();
    let Some(current) = previous.conversations.get(id) else {
        return (next, Vec::new());
    };
    let thread = match update.change.apply(current) {
        Ok(thread) => thread,
        Err(error) => {
            Arc::make_mut(&mut next.subscriptions).remove(id);
            next.error = Some(format!(
                "会話の更新を適用できないため再取得しています: {error}"
            ));
            return (next, vec![Effect::execute(op::ReadThread::new(id.clone()))]);
        }
    };
    use crate::session::SessionChange;
    let completed = matches!(
        update.change,
        SessionChange::Turn {
            completed: true,
            ..
        }
    );
    let active = match &update.change {
        SessionChange::Status { status } => {
            Some(status.kind == crate::models::ThreadStatusKind::Active)
        }
        SessionChange::Turn { .. } => Some(
            thread
                .status
                .as_ref()
                .is_some_and(|status| status.kind == crate::models::ThreadStatusKind::Active),
        ),
        _ => None,
    };
    if let Some(active) = active {
        let activity = Arc::make_mut(&mut next.activity);
        activity.active.insert(id.clone(), active);
        if active {
            activity.unread.remove(id);
        } else if matches!(&update.change, SessionChange::Turn { completed:true, turn } if turn.status.as_deref() == Some("completed"))
            && previous.navigation.thread_id.as_ref() != Some(id)
        {
            activity.unread.insert(id.clone());
        }
    }
    let refresh_workspace = (completed || matches!(update.change, SessionChange::Item { .. }))
        && current.cwd.as_deref() == Some(&next.navigation.cwd);
    Arc::make_mut(&mut next.conversations).insert(id.clone(), Arc::new(thread));
    project_requests(&mut next);
    reconcile_pending(&mut next, id);
    let changed_metadata = matches!(&update.change, SessionChange::Item { item, .. } if item.kind.as_deref() == Some("userMessage") || (item.kind.as_deref() == Some("commandExecution") && item.status.as_deref() == Some("completed")));
    let mut effects = if completed || changed_metadata {
        refresh_list(previous)
    } else {
        Vec::new()
    };
    if next.connected
        && completed
        && active == Some(false)
        && next
            .conversations
            .get(id)
            .is_none_or(|thread| thread.requests.is_empty())
        && next.navigation.thread_id.as_ref() != Some(id)
    {
        Arc::make_mut(&mut next.subscriptions).remove(id);
    }
    if refresh_workspace {
        effects.extend(op::review_workspace(&mut next));
    }
    (next, effects)
}
