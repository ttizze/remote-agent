//! Protocol notifications mutate the same immutable snapshot as RPC responses.
use super::*;
pub(super) fn notification(
    previous: &Snapshot,
    method: &str,
    params: Value,
) -> (Snapshot, Vec<Effect>) {
    if method == "host/session/activity" {
        let Some(session) = params
            .get("session")
            .cloned()
            .and_then(|value| serde_json::from_value::<crate::session::SessionRef>(value).ok())
        else {
            return (previous.clone(), Vec::new());
        };
        let Some(active) = params["active"].as_bool() else {
            return (previous.clone(), Vec::new());
        };
        let mut next = previous.clone();
        let id = session.thread_id();
        let activity = Arc::make_mut(&mut next.activity);
        activity.active.insert(id.clone(), active);
        if active {
            activity.unread.remove(&id);
        } else if params["finished"] == true && previous.navigation.thread_id.as_ref() != Some(&id)
        {
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
    if method == "host/session/update" {
        let update: crate::session::SessionUpdate = match serde_json::from_value(params) {
            Ok(update) => update,
            Err(error) => {
                return reduce(
                    previous,
                    Event::Failed(format!("invalid session update: {error}")),
                );
            }
        };
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
                return (
                    next,
                    vec![
                        Effect::execute(op::CloseSubscription {
                            subscription_id: update.subscription_id.to_string(),
                        }),
                        Effect::execute(op::ReadThread::new(id.clone())),
                    ],
                );
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
            && let Some(subscription) = Arc::make_mut(&mut next.subscriptions).remove(id)
        {
            effects.push(Effect::execute(op::CloseSubscription {
                subscription_id: subscription.to_string(),
            }));
        }
        if refresh_workspace {
            effects.extend(op::review_workspace(&mut next));
        }
        return (next, effects);
    }
    if method == "host/terminal/failed" {
        let (Some(handle), Some(reason)) =
            (params["processHandle"].as_str(), params["message"].as_str())
        else {
            return reduce(
                previous,
                Event::Failed("invalid terminal failure notification".into()),
            );
        };
        return reduce(
            previous,
            Event::TerminalFailed {
                handle: handle.into(),
                reason: reason.into(),
            },
        );
    }
    if matches!(
        method,
        "process/outputDelta"
            | "process/exited"
            | "host/terminal/restored"
            | "host/terminal/detached"
    ) {
        return process(previous, method, params);
    }
    if method == "thread/name/updated" {
        return (previous.clone(), refresh_list(previous));
    }
    (previous.clone(), Vec::new())
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

fn process(previous: &Snapshot, method: &str, params: Value) -> (Snapshot, Vec<Effect>) {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ProcessEvent {
        process_handle: String,
        delta_base64: Option<String>,
        exit_code: Option<i32>,
        cols: Option<u16>,
        rows: Option<u16>,
    }
    let params: ProcessEvent = match serde_json::from_value(params) {
        Ok(params) => params,
        Err(error) => {
            return reduce(
                previous,
                Event::Failed(format!("invalid {method} notification: {error}")),
            );
        }
    };
    let Some(current) = previous.terminals.get(&params.process_handle) else {
        return (previous.clone(), Vec::new());
    };
    if matches!(current.phase, TerminalPhase::Exited(_)) {
        return (previous.clone(), Vec::new());
    }
    let mut next = previous.clone();
    let Some(terminal) = shared_mut(&mut next.terminals, &params.process_handle) else {
        return (next, Vec::new());
    };
    if method == "host/terminal/detached" {
        terminal.phase = TerminalPhase::Detached;
    } else if method == "process/exited" {
        let Some(code) = params.exit_code else {
            return reduce(
                previous,
                Event::Failed("process exit code is missing".into()),
            );
        };
        terminal.phase = TerminalPhase::Exited(code);
    } else {
        let Some(data) = params.delta_base64 else {
            return reduce(previous, Event::Failed("process output is missing".into()));
        };
        use base64::Engine as _;
        let data = match base64::engine::general_purpose::STANDARD.decode(data) {
            Ok(data) => data,
            Err(error) => {
                return reduce(
                    previous,
                    Event::Failed(format!("invalid terminal bytes: {error}")),
                );
            }
        };
        let reset_size = if method == "host/terminal/restored" {
            let (Some(cols), Some(rows)) = (params.cols, params.rows) else {
                return reduce(
                    previous,
                    Event::Failed("terminal restoration size is missing".into()),
                );
            };
            terminal.output.clear();
            terminal.phase = TerminalPhase::Running;
            terminal.size = crate::client::TerminalSize { cols, rows };
            Some(terminal.size)
        } else {
            None
        };
        terminal.sequence += 1;
        terminal.output.push_back(Arc::new(TerminalOutput {
            sequence: terminal.sequence,
            data,
            reset_size,
        }));
    }
    (next, Vec::new())
}
