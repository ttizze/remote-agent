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
        let id = session.clone();
        let activity = Arc::make_mut(&mut next.activity);
        activity.active.insert(id.clone(), active);
        if active {
            activity.unread.remove(&id);
        } else if finished && previous.navigation.thread_id.as_ref() != Some(&id) {
            activity.unread.insert(id);
        }
        let child = previous
            .conversations
            .get(&session)
            .is_some_and(|thread| thread.parent_id.is_some());
        let listed = previous.threads.as_ref().is_some_and(|list| {
            list.data
                .iter()
                .any(|thread| thread.id.as_ref() == Some(&session))
        });
        let mut effects =
            if (child && previous.list_query.search_term.trim().is_empty()) || (active && listed) {
                Vec::new()
            } else {
                refresh_list(previous)
            };
        if !listed && !previous.conversations.contains_key(&session) {
            effects.extend(op::refresh_agents(
                previous.connected,
                previous.observed_agents.as_ref(),
            ));
        }
        return (next, effects);
    }

    match message {
        Notification::TerminalFailed { handle, reason } => {
            reduce(previous, Event::TerminalFailed { handle, reason })
        }
        event @ (Notification::Output { .. }
        | Notification::Exited { .. }
        | Notification::TerminalRestored { .. }
        | Notification::TerminalDetached { .. }) => process(previous, event),
        Notification::SessionRenamed { session } => {
            let effects = if previous
                .conversations
                .get(&session)
                .is_some_and(|thread| thread.parent_id.is_some())
            {
                let mut effects =
                    op::refresh_agents(previous.connected, previous.observed_agents.as_ref());
                if !previous.list_query.search_term.trim().is_empty() {
                    effects.extend(refresh_list(previous));
                }
                effects
            } else {
                refresh_list(previous)
            };
            (previous.clone(), effects)
        }
        _ => (previous.clone(), Vec::new()),
    }
}

fn refresh_list(snapshot: &Snapshot) -> Vec<Effect> {
    if snapshot.connected {
        vec![Effect::execute(op::ListSessions::new(
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
        Err(_) => {
            Arc::make_mut(&mut next.subscriptions).remove(id);
            // Recover through a fresh snapshot/subscription. The Store reports
            // a failed read; a recoverable gap is not a persistent user error.
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
    let details = if let SessionChange::TurnItems { turn_id, .. } = &update.change {
        thread
            .turns
            .iter()
            .flatten()
            .rfind(|turn| &turn.id == turn_id)
            .map_or_else(Vec::new, |turn| {
                op::item_details(id, std::slice::from_ref(turn))
            })
    } else {
        Vec::new()
    };
    let active = thread.status == crate::models::SessionStatus::Running;
    let refresh_workspace = (completed || matches!(update.change, SessionChange::Item { .. }))
        && current.cwd.as_deref() == Some(&next.navigation.cwd);
    Arc::make_mut(&mut next.conversations).insert(id.clone(), Arc::new(thread));
    reconcile_pending(&mut next, id);
    let changed_metadata = matches!(&update.change, SessionChange::Item { item, .. } if matches!(item.body(), crate::models::ItemBody::UserMessage { .. }) || (matches!(item.body(), crate::models::ItemBody::CommandExecution { .. }) && item.status == crate::models::ItemStatus::Completed));
    let mut effects = details;
    if (completed || changed_metadata)
        && (current.parent_id.is_none() || !previous.list_query.search_term.trim().is_empty())
    {
        effects.extend(refresh_list(previous));
    }
    if (previous.observed_agents.as_ref() == Some(id) || current.parent_id.is_some())
        && matches!(&update.change, SessionChange::Item { item, .. } if matches!(item.body(), crate::models::ItemBody::Subagent { .. }))
    {
        effects.extend(op::refresh_agents(
            previous.connected,
            previous.observed_agents.as_ref(),
        ));
    }
    if next.connected
        && completed
        && !active
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{ProviderKind, SessionRef};

    #[rstest::rstest]
    fn known_child_activity_updates_rows_without_refetching(
        #[values(true, false)] active: bool,
        #[values(true, false)] observing: bool,
    ) {
        let parent = SessionRef::new(ProviderKind::Codex, "parent".into()).unwrap();
        let child = SessionRef::new(ProviderKind::Codex, "child".into()).unwrap();
        let snapshot = Snapshot {
            connected: true,
            observed_agents: observing.then_some(parent.clone()),
            navigation: Arc::new(Navigation {
                thread_id: Some(parent.clone()),
                ..Default::default()
            }),
            conversations: Arc::new(
                [(
                    child.clone(),
                    Arc::new(crate::models::Thread {
                        id: Some(child.clone()),
                        parent_id: Some(parent),
                        name: Some("Review".into()),
                        ..Default::default()
                    }),
                )]
                .into(),
            ),
            ..Default::default()
        };
        let (next, effects) = notification(
            &snapshot,
            crate::protocol::Notification::Activity {
                session: child.clone(),
                active,
                finished: !active,
            },
        );
        assert!(effects.is_empty());
        assert_eq!(next.activity.active[&child], active);
        assert_eq!(next.agent_panel()[0].active, active);
        assert_eq!(next.navigation, snapshot.navigation);
    }

    #[test]
    fn running_subagents_refresh_only_the_observed_fleet_without_waiting_for_completion() {
        let parent = SessionRef::new(ProviderKind::Codex, "parent".into()).unwrap();
        let child = SessionRef::new(ProviderKind::Codex, "child".into()).unwrap();
        let subscription = uuid::Uuid::new_v4();
        let mut snapshot = Snapshot {
            connected: true,
            threads: Some(Arc::new(serde_json::from_value(serde_json::json!({
                "data":[{"id":parent}],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false
            })).unwrap())),
            subscriptions: Arc::new([(parent.clone(), subscription)].into()),
            conversations: Arc::new([(parent.clone(), Arc::new(crate::models::Thread {
                id: Some(parent.clone()),
                turns: Some(vec![Arc::new(crate::models::Turn { id:"turn".into(), status:crate::models::TurnStatus::Running, items:Some(vec![]), ..Default::default() })]),
                ..Default::default()
            }))].into()),
            ..Default::default()
        };
        for (session, expected_refreshes) in [(parent.clone(), 0), (child.clone(), 1)] {
            let (next, effects) = notification(
                &snapshot,
                crate::protocol::Notification::Activity {
                    session: session.clone(),
                    active: true,
                    finished: false,
                },
            );
            assert_eq!(effects.len(), expected_refreshes);
            assert!(next.activity.active[&session]);
            assert_eq!(next.navigation, snapshot.navigation);
        }
        let item = serde_json::from_value(serde_json::json!({
            "id":"spawn","status":"running","clientInputId":null,
            "body":{"inline":{"body":{"subagent":{"tool":"spawnAgent","prompt":null,"model":null,"effort":null,"sender":parent,"receivers":[child],"states":[],"agentId":null,"result":null}}}}
        })).unwrap();
        snapshot.observed_agents = Some(parent);
        let (next, effects) = session_update(
            &snapshot,
            crate::session::SessionUpdate {
                subscription_id: subscription,
                change: crate::session::SessionChange::Item {
                    turn_id: "turn".into(),
                    item: Arc::new(item),
                },
            },
        );
        assert_eq!(
            effects.len(),
            1,
            "a spawn refreshes while the parent is still running"
        );
        assert_eq!(next.navigation, snapshot.navigation);
    }
}
