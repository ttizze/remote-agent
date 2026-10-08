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
        let activity_changed = previous.activity.active.get(&session) != Some(&active);
        let was_unread = previous.activity.unread.contains(&session);
        let unread = !active
            && (was_unread
                || (finished && previous.navigation.thread_id.as_ref() != Some(&session)));
        let mut next = previous.clone();
        if activity_changed || was_unread != unread {
            let activity = Arc::make_mut(&mut next.activity);
            activity.active.insert(session.clone(), active);
            if unread {
                activity.unread.insert(session.clone());
            } else {
                activity.unread.remove(&session);
            }
        }
        let listed = previous.threads.as_ref().is_some_and(|list| {
            list.data
                .iter()
                .any(|thread| thread.id.as_ref() == Some(&session))
        });
        return (
            next,
            // Discover unseen sessions and check Git when work ends. Known
            // running sessions and repeated idle notices need no list read.
            if !listed || (!active && activity_changed) {
                refresh_list(previous.connected, &previous.list_query)
            } else {
                Vec::new()
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
        Notification::SessionRenamed { .. } => (
            previous.clone(),
            refresh_list(previous.connected, &previous.list_query),
        ),
        _ => (previous.clone(), Vec::new()),
    }
}

fn refresh_list(connected: bool, query: &ListQuery) -> Vec<Effect> {
    if connected {
        vec![Effect::execute(op::ListSessions::new(query.clone()))]
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
    let changed_metadata = match &update.change {
        SessionChange::Item { item, .. } => match item.body() {
            crate::models::ItemBody::UserMessage { .. } => true,
            crate::models::ItemBody::Subagent { receivers, .. } => {
                receivers.iter().any(|receiver| {
                    !previous.threads.as_ref().is_some_and(|list| {
                        list.data
                            .iter()
                            .any(|thread| thread.id.as_ref() == Some(receiver))
                    })
                })
            }
            _ => false,
        },
        _ => false,
    };
    let mut effects = details;
    // The Host broadcasts Activity for turn/status changes, including to
    // clients without a conversation subscription. It owns the end-of-work
    // list refresh; streamed commands and known agent updates stay local.
    if changed_metadata {
        effects.extend(refresh_list(previous.connected, &previous.list_query));
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
    use crate::protocol::Notification;
    use crate::session::{ProviderKind, SessionRef};
    use proptest::prelude::*;
    use serde_json::json;

    fn snapshot(provider: ProviderKind) -> Snapshot {
        let parent = SessionRef::new(provider, "parent".into()).unwrap();
        let subscription = uuid::Uuid::new_v4();
        Snapshot {
            connected: true,
            threads: Some(Arc::new(serde_json::from_value(json!({
                "data":[{"id":parent,"name":"Task","status":"idle","worktreeStatus":"unmerged"}],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false
            })).unwrap())),
            subscriptions: Arc::new([(parent.clone(), subscription)].into()),
            conversations: Arc::new([(parent.clone(), Arc::new(crate::models::Thread {
                id: Some(parent.clone()),
                turns: Some(vec![Arc::new(crate::models::Turn { id:"turn".into(), status:crate::models::TurnStatus::Running, items:Some(vec![]), ..Default::default() })]),
                ..Default::default()
            }))].into()),
            ..Default::default()
        }
    }

    proptest! {
        #[test]
        fn activity_updates_refresh_once_per_run_and_preserve_cached_rows(
            claude in any::<bool>(),
            connected in any::<bool>(),
            selected in any::<bool>(),
            finished in any::<bool>(),
            repetitions in 1usize..12,
        ) {
            let mut state = snapshot(if claude { ProviderKind::Claude } else { ProviderKind::Codex });
            state.connected = connected;
            let id = state.threads.as_ref().unwrap().data[0].id.clone().unwrap();
            if selected {
                Arc::make_mut(&mut state.navigation).thread_id = Some(id.clone());
            }
            Arc::make_mut(&mut state.activity).unread.insert(id.clone());
            let cached_list = state.threads.clone().unwrap();
            for _ in 0..2 {
                let (running, effects) = notification(&state, Notification::Activity {
                    session: id.clone(), active: true, finished: false,
                });
                prop_assert!(effects.is_empty());
                prop_assert!(running.thread_list().unwrap().threads[0].active);
                prop_assert!(!running.thread_list().unwrap().threads[0].unread);
                state = running;
                for _ in 0..repetitions {
                    let (repeated, effects) = notification(&state, Notification::Activity {
                        session: id.clone(), active: true, finished: false,
                    });
                    prop_assert!(effects.is_empty());
                    prop_assert!(Arc::ptr_eq(&state.activity, &repeated.activity));
                    state = repeated;
                }
                let (idle, effects) = notification(&state, Notification::Activity {
                    session: id.clone(), active: false, finished: false,
                });
                prop_assert_eq!(effects.len(), usize::from(connected));
                state = idle;
                for _ in 0..repetitions {
                    let (ended, effects) = notification(&state, Notification::Activity {
                        session: id.clone(), active: false, finished,
                    });
                    prop_assert!(effects.is_empty());
                    let row = &ended.thread_list().unwrap().threads[0];
                    prop_assert!(!row.active);
                    prop_assert_eq!(row.unread, finished && !selected);
                    prop_assert_eq!(row.title.as_str(), "Task");
                    prop_assert_eq!(row.worktree_status, Some(crate::models::WorktreeStatus::Unmerged));
                    prop_assert_eq!(&state.navigation, &ended.navigation);
                    state = ended;
                }
                prop_assert!(Arc::ptr_eq(state.threads.as_ref().unwrap(), &cached_list));
                let (repeated, effects) = notification(&state, Notification::Activity {
                    session: id.clone(), active: false, finished,
                });
                prop_assert!(effects.is_empty());
                prop_assert!(Arc::ptr_eq(&state.activity, &repeated.activity));
            }
        }
    }

    #[rstest::rstest]
    #[case::codex(ProviderKind::Codex)]
    #[case::claude(ProviderKind::Claude)]
    fn streamed_commands_and_known_agents_do_not_reload_titles(#[case] provider: ProviderKind) {
        let mut state = snapshot(provider);
        let id = state.threads.as_ref().unwrap().data[0].id.clone().unwrap();
        let subscription = state.subscriptions[&id];
        let child = SessionRef::new(provider, "child".into()).unwrap();
        Arc::make_mut(state.threads.as_mut().unwrap())
            .data
            .push(crate::models::Thread {
                id: Some(child.clone()),
                parent_id: Some(id.clone()),
                ..Default::default()
            });
        state = notification(
            &state,
            Notification::Activity {
                session: id.clone(),
                active: true,
                finished: false,
            },
        )
        .0;
        for body in [
            json!({"commandExecution":{"command":"git status","cwd":null,"output":"","exitCode":0}}),
            json!({"subagent":{"tool":"wait","prompt":null,"model":null,"effort":null,"sender":id,"receivers":[child],"states":[],"agentId":null,"result":null}}),
        ] {
            for status in ["running", "completed"] {
                let item = serde_json::from_value(json!({
                    "id":"work","status":status,"clientInputId":null,
                    "body":{"inline":{"body":body}}
                }))
                .unwrap();
                let (next, effects) = session_update(
                    &state,
                    crate::session::SessionUpdate {
                        subscription_id: subscription,
                        change: crate::session::SessionChange::Item {
                            turn_id: "turn".into(),
                            item: Arc::new(item),
                        },
                    },
                );
                assert!(effects.is_empty());
                assert!(next.thread_list().unwrap().threads[0].active);
                state = next;
            }
        }
        let (ended, effects) = session_update(
            &state,
            crate::session::SessionUpdate {
                subscription_id: subscription,
                change: crate::session::SessionChange::Turn {
                    turn: crate::models::Turn {
                        id: "turn".into(),
                        status: crate::models::TurnStatus::Completed,
                        ..Default::default()
                    },
                    completed: true,
                },
            },
        );
        assert!(
            effects.is_empty(),
            "the global Activity notice owns the completion refresh"
        );
        assert!(!ended.subscriptions.contains_key(&id));
        let (ended, effects) = notification(
            &ended,
            Notification::Activity {
                session: id.clone(),
                active: false,
                finished: true,
            },
        );
        assert_eq!(
            effects.len(),
            1,
            "completion still refreshes Git and metadata"
        );
        let row = &ended.thread_list().unwrap().threads[0];
        assert!(!row.active);
        assert!(row.unread);
    }

    #[test]
    fn user_messages_and_renames_still_refresh_titles() {
        let state = snapshot(ProviderKind::Codex);
        let id = state.threads.as_ref().unwrap().data[0].id.clone().unwrap();
        let item = serde_json::from_value(json!({
            "id":"user","status":"completed","clientInputId":null,
            "body":{"inline":{"body":{"userMessage":{"text":"New task title","content":[]}}}}
        }))
        .unwrap();
        let (_, effects) = session_update(
            &state,
            crate::session::SessionUpdate {
                subscription_id: state.subscriptions[&id],
                change: crate::session::SessionChange::Item {
                    turn_id: "turn".into(),
                    item: Arc::new(item),
                },
            },
        );
        assert_eq!(effects.len(), 1);
        let (_, effects) = notification(&state, Notification::SessionRenamed { session: id });
        assert_eq!(effects.len(), 1);
    }

    #[test]
    fn running_subagents_refresh_the_list_without_waiting_for_completion() {
        let snapshot = snapshot(ProviderKind::Codex);
        let parent = snapshot.threads.as_ref().unwrap().data[0]
            .id
            .clone()
            .unwrap();
        let child = SessionRef::new(ProviderKind::Codex, "child".into()).unwrap();
        let subscription = snapshot.subscriptions[&parent];
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
