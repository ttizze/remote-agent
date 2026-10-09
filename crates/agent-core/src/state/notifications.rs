//! Protocol notifications mutate the same immutable snapshot as RPC responses.
use super::*;
use crate::session::SessionRef;
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
        let listed = previous
            .listed_threads()
            .any(|thread| thread.id.as_ref() == Some(&session));
        let child = previous
            .conversations
            .get(&session)
            .is_some_and(|thread| thread.parent_id.is_some());
        let mut effects = Vec::new();
        if previous.connected
            && !child
            && activity_changed
            && !active
            && listed
            && let Some(thread) = previous
                .listed_threads()
                .find(|thread| thread.id.as_ref() == Some(&session))
            && let Some(cwd) = &thread.cwd
        {
            effects.push(Effect::execute(op::ReadListDecorations {
                scope: agent_protocol::operations::ListDecorationScope::Task {
                    session: session.clone(),
                },
                project_ids: Vec::new(),
                threads: vec![(session.clone(), cwd.clone(), thread.git_branch.clone())],
            }));
        }
        if activity_changed && !listed && !previous.conversations.contains_key(&session) {
            effects.extend(op::refresh_agents(
                previous.connected,
                previous.observed_agents.as_ref(),
            ));
        }
        return (next, effects);
    }

    match message {
        Notification::TaskActivity { state } => {
            let mut next = previous.clone();
            next.accept_task_activity(state);
            (next, Vec::new())
        }
        Notification::TerminalFailed { handle, reason } => {
            reduce(previous, Event::TerminalFailed { handle, reason })
        }
        event @ (Notification::Output { .. }
        | Notification::Exited { .. }
        | Notification::TerminalRestored { .. }
        | Notification::TerminalDetached { .. }) => process(previous, event),
        Notification::SessionRenamed {
            session,
            name,
            revision,
        } => {
            let mut next = previous.clone();
            next.rename_session(&session, name.as_deref(), revision);
            (next, Vec::new())
        }
        Notification::SessionUpdated { thread, project } => {
            let mut next = previous.clone();
            next.receive_session_summary(*thread, project);
            (next, Vec::new())
        }
        _ => (previous.clone(), Vec::new()),
    }
}

impl Snapshot {
    fn rename_session(&mut self, session: &SessionRef, name: Option<&str>, revision: u64) {
        if let Some(thread) = self.conversations.get(session)
            && thread.list_revision < revision
        {
            let thread = Arc::make_mut(
                Arc::make_mut(&mut self.conversations)
                    .get_mut(session)
                    .unwrap(),
            );
            thread.name = name.map(str::to_owned);
            thread.list_revision = revision;
        }
        if let Some(page) = &mut self.threads
            && let Some(data) = renamed_rows(&page.data, session, name, revision)
        {
            Arc::make_mut(page).data = data;
        }
        if self.project_threads.values().any(|page| {
            page.data
                .iter()
                .any(|row| row.id.as_ref() == Some(session) && row.list_revision < revision)
        }) {
            for page in Arc::make_mut(&mut self.project_threads).values_mut() {
                if let Some(data) = renamed_rows(&page.data, session, name, revision) {
                    Arc::make_mut(page).data = data;
                }
            }
        }
    }
    fn receive_session_summary(
        &mut self,
        mut thread: Thread,
        project: Option<crate::models::Project>,
    ) {
        let Some(id) = thread.id.as_ref() else { return };
        if let Some(state) = &self.task_activity
            && let Some((_, status)) = state.statuses.iter().find(|(session, _)| session == id)
        {
            thread.status = *status;
        }
        if let Some(cached) = self.conversations.get(id)
            && cached.list_revision <= thread.list_revision
            && cached
                .name
                .as_deref()
                .is_none_or(|name| name.trim().is_empty())
            && thread.name.is_some()
        {
            let cached = Arc::make_mut(Arc::make_mut(&mut self.conversations).get_mut(id).unwrap());
            cached.name = thread.name.clone();
            cached.list_revision = thread.list_revision;
        }
        if thread.parent_id.is_some() {
            return;
        }
        let inserting = thread
            .name
            .as_deref()
            .unwrap_or("")
            .to_lowercase()
            .contains(&self.list_query.search_term.trim().to_lowercase());
        if let Some(project) = project
            && let Some(page) = &mut self.threads
        {
            let position = page.projects.iter().position(|old| old.id == project.id);
            if (position.is_some() || inserting)
                && position
                    .is_none_or(|index| page.projects[index].list_revision < project.list_revision)
            {
                let page = Arc::make_mut(page);
                let mut project = project;
                if let Some(index) = position {
                    project.favicon_png = page.projects.remove(index).favicon_png;
                }
                page.projects.insert(0, project);
                page.has_more_projects |=
                    page.projects.len() > self.list_query.project_limit as usize;
                page.projects
                    .truncate(self.list_query.project_limit as usize);
            }
        }
        if let Some(project) = thread.project_id.as_ref() {
            if let Some(page) = self.project_threads.get(project)
                && let Some((data, more)) = updated_rows(&page.data, &thread, inserting, page.limit)
            {
                let page = Arc::make_mut(
                    Arc::make_mut(&mut self.project_threads)
                        .get_mut(project)
                        .unwrap(),
                );
                page.data = data;
                page.has_more |= more;
            }
        } else if let Some(page) = &mut self.threads
            && let Some((data, more)) = updated_rows(&page.data, &thread, inserting, page.limit)
        {
            let page = Arc::make_mut(page);
            page.data = data;
            page.has_more |= more;
        }
    }
}
fn renamed_rows(
    rows: &[Thread],
    session: &SessionRef,
    name: Option<&str>,
    revision: u64,
) -> Option<Vec<Thread>> {
    let index = rows
        .iter()
        .position(|row| row.id.as_ref() == Some(session) && row.list_revision < revision)?;
    let mut rows = rows.to_vec();
    rows[index].name = name.map(str::to_owned);
    rows[index].list_revision = revision;
    Some(rows)
}
fn updated_rows(
    rows: &[Thread],
    thread: &Thread,
    insert: bool,
    limit: u32,
) -> Option<(Vec<Thread>, bool)> {
    let index = rows.iter().position(|row| row.id == thread.id);
    if index.is_none() && !insert
        || index.is_some_and(|index| rows[index].list_revision > thread.list_revision)
    {
        return None;
    }
    let mut thread = thread.clone();
    if let Some(old) = index.map(|index| &rows[index]) {
        // Rename notices own existing names; submission metadata may have been
        // read before a native automatic rename.
        thread.name = old
            .name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .or(thread.name);
        if old.cwd == thread.cwd {
            thread.git_branch = old.git_branch.clone().or(thread.git_branch);
            thread.worktree_status = old.worktree_status;
        }
    }
    if index.is_some_and(|index| rows[index] == thread) {
        return None;
    }
    let mut rows = rows.to_vec();
    if let Some(index) = index {
        rows[index] = thread;
    } else {
        rows.push(thread);
    }
    Some(operations::page_rows(rows, limit))
}

fn fleet_contains(
    root: Option<&SessionRef>,
    session: &SessionRef,
    conversations: &BTreeMap<SessionRef, Arc<Thread>>,
) -> bool {
    let Some(root) = root else {
        return false;
    };
    let mut ancestor = session;
    // The number of known sessions also bounds malformed parent cycles.
    for _ in 0..=conversations.len() {
        if ancestor.provider != root.provider {
            return false;
        }
        if ancestor == root {
            return true;
        }
        let Some(parent) = conversations
            .get(ancestor)
            .and_then(|thread| thread.parent_id.as_ref())
        else {
            return false;
        };
        ancestor = parent;
    }
    false
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
    let mut effects = details;
    if fleet_contains(
        previous.observed_agents.as_ref(),
        id,
        &previous.conversations,
    ) && matches!(&update.change, SessionChange::Item { item, .. }
            if matches!(item.body(), crate::models::ItemBody::Subagent { receivers, .. }
                if receivers.iter().any(|receiver| !previous.conversations.contains_key(receiver))))
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
    use crate::protocol::Notification;
    use crate::session::{ProviderKind, SessionRef};
    use proptest::prelude::*;
    use serde_json::json;

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

    fn snapshot(provider: ProviderKind) -> Snapshot {
        let parent = SessionRef::new(provider, "parent".into()).unwrap();
        let subscription = uuid::Uuid::new_v4();
        Snapshot {
            connected: true,
            threads: Some(Arc::new(serde_json::from_value(json!({
                "data":[{"id":parent,"name":"Task","cwd":"/fixture","status":"idle","worktreeStatus":"unmerged"}],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,})).unwrap())),
            subscriptions: Arc::new([(parent.clone(), subscription)].into()),
            conversations: Arc::new([(parent.clone(), Arc::new(crate::models::Thread {
                id: Some(parent.clone()),
                turns: Some(vec![Arc::new(crate::models::Turn { id:"turn".into(), status:crate::models::TurnStatus::Running, items:Some(vec![]), ..Default::default() })]),
                ..Default::default()
            }))].into()),
            ..Default::default()
        }
    }

    #[rstest::rstest]
    fn unknown_activity_does_not_repeat_discovery_reads(
        #[values(true, false)] active: bool,
        #[values(true, false)] observing: bool,
    ) {
        let mut state = snapshot(ProviderKind::Codex);
        let parent = state.threads.as_ref().unwrap().data[0].id.clone().unwrap();
        Arc::make_mut(&mut state.navigation).thread_id = Some(parent.clone());
        state.observed_agents = observing.then_some(parent);
        let unknown = SessionRef::new(ProviderKind::Codex, "unseen".into()).unwrap();
        let (discovered, effects) = notification(
            &state,
            Notification::Activity {
                session: unknown.clone(),
                active,
                finished: false,
            },
        );
        assert_eq!(effects.len(), usize::from(observing));
        let (repeated, effects) = notification(
            &discovered,
            Notification::Activity {
                session: unknown,
                active,
                finished: false,
            },
        );
        assert!(effects.is_empty());
        assert!(Arc::ptr_eq(&discovered.activity, &repeated.activity));
        assert_eq!(repeated.navigation, state.navigation);
    }

    #[test]
    fn unrelated_fleets_do_not_trigger_reads_of_the_visible_fleet() {
        let mut state = snapshot(ProviderKind::Codex);
        let parent = state.threads.as_ref().unwrap().data[0].id.clone().unwrap();
        state.observed_agents = Some(parent.clone());
        Arc::make_mut(&mut state.navigation).thread_id = Some(parent);
        let session = |id: &str| SessionRef::new(ProviderKind::Codex, id.into()).unwrap();
        for (id, parent) in [
            ("child", "parent"),
            ("grandchild", "child"),
            ("unrelated", "other"),
            ("cycle-a", "cycle-b"),
            ("cycle-b", "cycle-a"),
        ] {
            let id = session(id);
            Arc::make_mut(&mut state.subscriptions).insert(id.clone(), uuid::Uuid::new_v4());
            Arc::make_mut(&mut state.conversations).insert(
                id.clone(),
                Arc::new(Thread {
                    id: Some(id),
                    parent_id: Some(session(parent)),
                    turns: Some(vec![Arc::new(crate::models::Turn {
                        id: "turn".into(),
                        status: crate::models::TurnStatus::Running,
                        items: Some(vec![]),
                        ..Default::default()
                    })]),
                    ..Default::default()
                }),
            );
        }
        let cross_provider =
            SessionRef::new(ProviderKind::Claude, "cross-provider".into()).unwrap();
        Arc::make_mut(&mut state.subscriptions)
            .insert(cross_provider.clone(), uuid::Uuid::new_v4());
        let mut cross_thread = state.conversations[&session("child")].as_ref().clone();
        cross_thread.id = Some(cross_provider.clone());
        Arc::make_mut(&mut state.conversations)
            .insert(cross_provider.clone(), Arc::new(cross_thread));
        for (id, expected) in [
            ("child", 1),
            ("grandchild", 1),
            ("unrelated", 0),
            ("cycle-a", 0),
        ]
        .into_iter()
        .map(|(id, expected)| (session(id), expected))
        .chain([(cross_provider, 0)])
        {
            let (_, effects) = notification(
                &state,
                Notification::SessionRenamed {
                    session: id.clone(),
                    name: Some("Renamed".into()),
                    revision: 1,
                },
            );
            assert!(effects.is_empty(), "rename {id} must be applied locally");
            let item = serde_json::from_value(json!({
                "id":"spawn","status":"running","clientInputId":null,
                "body":{"inline":{"body":{"subagent":{"tool":"spawnAgent","prompt":null,"model":null,"effort":null,"sender":id,"receivers":[session("new-agent")],"states":[],"agentId":null,"result":null}}}}
            })).unwrap();
            let (next, effects) = session_update(
                &state,
                crate::session::SessionUpdate {
                    subscription_id: state.subscriptions[&id],
                    change: crate::session::SessionChange::Item {
                        turn_id: "turn".into(),
                        item: Arc::new(item),
                    },
                },
            );
            assert_eq!(effects.len(), expected, "spawn {id}");
            assert_eq!(next.navigation, state.navigation);
        }
    }

    proptest! {
        #[test]
        fn activity_updates_only_decorate_once_per_run_and_preserve_cached_rows(
            claude in any::<bool>(),
            connected in any::<bool>(),
            selected in any::<bool>(),
            finished in any::<bool>(),
            repetitions in 1usize..12,
        ) {
            let mut state = snapshot(if claude { ProviderKind::Claude } else { ProviderKind::Codex });
            state.connected = connected;
            Arc::make_mut(state.threads.as_mut().unwrap()).data[0].cwd = Some("/fixture".into());
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
        Arc::make_mut(state.threads.as_mut().unwrap()).data[0].cwd = Some("/fixture".into());
        let id = state.threads.as_ref().unwrap().data[0].id.clone().unwrap();
        let subscription = state.subscriptions[&id];
        let child = SessionRef::new(provider, "child".into()).unwrap();
        state.observed_agents = Some(id.clone());
        Arc::make_mut(&mut state.conversations).insert(
            child.clone(),
            Arc::new(crate::models::Thread {
                id: Some(child.clone()),
                parent_id: Some(id.clone()),
                ..Default::default()
            }),
        );
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
    fn user_messages_and_renames_do_not_read_lists() {
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
        assert!(effects.is_empty());
        let (next, effects) = notification(
            &state,
            Notification::SessionRenamed {
                session: id.clone(),
                name: Some("Renamed".into()),
                revision: 1,
            },
        );
        assert!(effects.is_empty());
        assert_eq!(next.conversations[&id].name.as_deref(), Some("Renamed"));
        assert_eq!(
            next.threads.as_ref().unwrap().data[0].name.as_deref(),
            Some("Renamed")
        );
        let (cleared, effects) = notification(
            &next,
            Notification::SessionRenamed {
                session: id.clone(),
                name: None,
                revision: 2,
            },
        );
        assert!(effects.is_empty());
        assert!(cleared.conversations[&id].name.is_none());
        assert!(cleared.threads.as_ref().unwrap().data[0].name.is_none());
    }

    proptest! {
        #[test]
        fn metadata_notices_preserve_closed_page_quota_and_newer_names(limit in prop_oneof![Just(5u32), Just(15u32), Just(25u32)]) {
            let mut state = snapshot(ProviderKind::Codex);
            let mut page = state.threads.as_ref().unwrap().as_ref().clone();
            page.limit = limit;
            page.data = (0..limit).map(|index| Thread {
                id: Some(SessionRef::new(ProviderKind::Codex, format!("task-{index}")).unwrap()),
                project_id: crate::models::ProjectMembership::Assigned("closed".into()),
                name: Some(format!("Task {index}")),
                updated_at: Some(f64::from(index)),
                ..Default::default()
            }).collect();
            state.project_threads = Arc::new([("closed".into(), Arc::new(page))].into());
            let id = state.project_threads["closed"].data[0].id.clone().unwrap();
            let (renamed, effects) = notification(&state, Notification::SessionRenamed { session: id.clone(), name: Some("New title".into()), revision: 2 });
            prop_assert!(effects.is_empty());
            let (next, effects) = notification(&renamed, Notification::SessionUpdated {
                thread: Thread { id: Some(id.clone()), project_id: crate::models::ProjectMembership::Assigned("closed".into()), name: Some("Old native name".into()), updated_at: Some(999.0), list_revision: 3, ..Default::default() }.into(), project: None,
            });
            prop_assert!(effects.is_empty());
            prop_assert!(next.expanded_projects.is_empty());
            prop_assert_eq!(next.project_threads["closed"].limit, limit);
            prop_assert_eq!(next.project_threads["closed"].data.len(), limit as usize);
            prop_assert!(!next.project_threads["closed"].has_more, "updating an existing row cannot invent a lookahead row");
            prop_assert_eq!(next.project_threads["closed"].data[0].name.as_deref(), Some("New title"));
            let (stale, effects) = notification(&next, Notification::SessionRenamed { session: id, name: Some("Stale".into()), revision: 1 });
            prop_assert!(effects.is_empty());
            prop_assert!(Arc::ptr_eq(&stale.project_threads, &next.project_threads));
            let (stale, effects) = notification(&next, Notification::SessionUpdated { thread: state.project_threads["closed"].data[0].clone().into(), project: None });
            prop_assert!(effects.is_empty());
            prop_assert!(Arc::ptr_eq(&stale.project_threads, &next.project_threads));
            let (added, effects) = notification(&next, Notification::SessionUpdated {
                thread: Thread { id: Some(SessionRef::new(ProviderKind::Codex, "new".into()).unwrap()), project_id: crate::models::ProjectMembership::Assigned("closed".into()), name: Some("Created task".into()), updated_at: Some(1000.0), list_revision: 4, ..Default::default() }.into(), project: None,
            });
            prop_assert!(effects.is_empty());
            prop_assert_eq!(added.project_threads["closed"].data.len(), limit as usize);
            prop_assert!(added.project_threads["closed"].has_more);
            prop_assert_eq!(added.project_threads["closed"].data[0].name.as_deref(), Some("Created task"));
        }
    }

    #[test]
    fn new_project_notice_does_not_claim_its_unreceived_page_is_loaded() {
        let state = snapshot(ProviderKind::Codex);
        let project = crate::models::Project {
            id: "unread".into(),
            name: "Unread".into(),
            list_revision: 1,
            ..Default::default()
        };
        let (next, effects) = notification(
            &state,
            Notification::SessionUpdated {
                thread: Thread {
                    id: Some(SessionRef::new(ProviderKind::Codex, "new".into()).unwrap()),
                    project_id: crate::models::ProjectMembership::Assigned("unread".into()),
                    list_revision: 1,
                    ..Default::default()
                }
                .into(),
                project: Some(project),
            },
        );
        assert!(effects.is_empty());
        assert_eq!(next.threads.as_ref().unwrap().projects[0].id, "unread");
        assert!(!next.project_threads.contains_key("unread"));
        let (_, effects) = reduce_intent(
            &next,
            Intent::SetProjectExpanded {
                project_id: "unread".into(),
                expanded: true,
            },
        );
        assert_eq!(
            effects.len(),
            1,
            "opening still reads a complete first page"
        );
    }

    #[test]
    fn running_subagents_refresh_only_the_observed_fleet_without_waiting_for_completion() {
        let mut snapshot = snapshot(ProviderKind::Codex);
        let parent = snapshot.threads.as_ref().unwrap().data[0]
            .id
            .clone()
            .unwrap();
        let child = SessionRef::new(ProviderKind::Codex, "child".into()).unwrap();
        let subscription = snapshot.subscriptions[&parent];
        for session in [parent.clone(), child.clone()] {
            let (next, effects) = notification(
                &snapshot,
                crate::protocol::Notification::Activity {
                    session: session.clone(),
                    active: true,
                    finished: false,
                },
            );
            assert!(effects.is_empty(), "activity alone must not read lists");
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
