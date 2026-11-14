//! Protocol notifications mutate the same immutable snapshot as RPC responses.
use super::*;
use crate::session::SessionRef;
pub(super) fn notification(
    previous: &Snapshot,
    message: crate::protocol::Notification,
) -> (Snapshot, Vec<Effect>) {
    use crate::protocol::Notification;
    if let Notification::Activity { session, active } = message {
        let activity_changed = previous.lists.activity.active.get(&session) != Some(&active);
        let mut next = previous.clone();
        if activity_changed {
            Arc::make_mut(&mut next.lists.activity)
                .active
                .insert(session.clone(), active);
        }
        let listed = previous
            .listed_threads()
            .find(|thread| thread.id.as_ref() == Some(&session));
        let child = previous
            .sessions
            .conversations
            .get(&session)
            .is_some_and(|thread| thread.parent_id.is_some());
        let mut effects = Vec::new();
        if previous.connection.connected
            && !child
            && activity_changed
            && !active
            && let Some(thread) = listed
            && let Some(cwd) = &thread.cwd
        {
            effects.push(Effect::execute(op::ReadListDecorations {
                scope: agent_protocol::operations::ListDecorationScope::Task {
                    session: session.clone(),
                },
                projects: Vec::new(),
                threads: vec![(session.clone(), cwd.clone(), thread.git_branch.clone())],
            }));
        }
        if activity_changed
            && listed.is_none()
            && !previous.sessions.conversations.contains_key(&session)
        {
            effects.extend(op::refresh_agents(
                previous.connection.connected,
                previous.panes.primary().observed_agents.as_ref(),
            ));
        }
        return (next, effects);
    }

    match message {
        Notification::ProjectClones { state } => {
            let mut next = previous.clone();
            let effects = next.accept_project_clones(state);
            (next, effects)
        }
        Notification::SessionHistoryReplaced { thread } => {
            let mut next = previous.clone();
            let effects = op::replace_history(&mut next, "", *thread, false);
            (next, effects)
        }
        Notification::ProviderSettings { settings } => {
            let mut next = previous.clone();
            let effects = if next.accept_provider_settings(settings) {
                op::refresh_models()
            } else {
                Vec::new()
            };
            (next, effects)
        }
        Notification::TaskActivity { state } => {
            let mut next = previous.clone();
            let effects = next.accept_task_activity(state);
            (next, effects)
        }
        Notification::TerminalFailed { handle, reason } => {
            reduce(previous, "", Event::TerminalFailed { handle, reason })
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
            let changed = thread
                .id
                .as_ref()
                .filter(|id| {
                    previous.connection.connected
                        && previous.sessions.subscriptions.contains_key(*id)
                })
                .filter(|id| {
                    previous.thread_metadata(id).is_some_and(|old| {
                        old.active_provider.unwrap_or(id.provider)
                            != thread.active_provider.unwrap_or(id.provider)
                    })
                })
                .cloned();
            next.receive_session_summary(*thread, project);
            if let Some(id) = &changed
                && let Some(cached) = shared_mut(&mut next.sessions.conversations, id)
            {
                cached.history_cursor = None;
                cached.history_has_more = Some(false);
            }
            let effects = changed
                .into_iter()
                .map(|id| Effect::execute(op::ReadThread::new(id)))
                .collect();
            (next, effects)
        }
        _ => (previous.clone(), Vec::new()),
    }
}

impl Snapshot {
    fn rename_session(&mut self, session: &SessionRef, name: Option<&str>, revision: u64) {
        if let Some(thread) = self.sessions.conversations.get(session)
            && thread.list_revision < revision
        {
            let thread = Arc::make_mut(
                Arc::make_mut(&mut self.sessions.conversations)
                    .get_mut(session)
                    .unwrap(),
            );
            thread.name = name.map(str::to_owned);
            thread.list_revision = revision;
        }
        if let Some(page) = &mut self.lists.threads
            && let Some(index) = rename_index(&page.data, session, revision)
        {
            apply_rename(page, index, name, revision);
        }
        let renames: Vec<_> = self
            .lists
            .project_threads
            .iter()
            .filter_map(|(project, page)| {
                rename_index(&page.data, session, revision).map(|index| (project.clone(), index))
            })
            .collect();
        if !renames.is_empty() {
            let pages = Arc::make_mut(&mut self.lists.project_threads);
            for (project, index) in renames {
                apply_rename(pages.get_mut(&project).unwrap(), index, name, revision);
            }
        }
    }
    fn receive_session_summary(
        &mut self,
        mut thread: Thread,
        project: Option<crate::models::Project>,
    ) {
        let Some(id) = thread.id.as_ref() else { return };
        if let Some(state) = &self.lists.task_activity
            && let Some((_, status)) = state.statuses.iter().find(|(session, _)| session == id)
        {
            thread.status = *status;
        }
        if let Some(cached) = self.sessions.conversations.get(id)
            && cached.list_revision <= thread.list_revision
            && cached
                .name
                .as_deref()
                .is_none_or(|name| name.trim().is_empty())
            && thread.name.is_some()
        {
            let cached = Arc::make_mut(
                Arc::make_mut(&mut self.sessions.conversations)
                    .get_mut(id)
                    .unwrap(),
            );
            cached.name = thread.name.clone();
            cached.list_revision = thread.list_revision;
        }
        if thread.parent_id.is_some() {
            return;
        }
        let search = self.lists.list_query.search_term.trim();
        let inserting = search.is_empty()
            || thread
                .name
                .as_deref()
                .unwrap_or("")
                .to_lowercase()
                .contains(&search.to_lowercase());
        if let Some(project) = project
            && let Some(page) = &mut self.lists.threads
        {
            let position = page.projects.iter().position(|old| old.id == project.id);
            if (position.is_some() || inserting)
                && position
                    .is_none_or(|index| page.projects[index].list_revision < project.list_revision)
            {
                let page = Arc::make_mut(page);
                let mut project = project;
                if let Some(index) = position {
                    project.icon = page.projects.remove(index).icon;
                }
                page.projects.insert(0, project);
                page.has_more_projects |=
                    page.projects.len() > self.lists.list_query.project_limit as usize;
                page.projects
                    .truncate(self.lists.list_query.project_limit as usize);
            }
        }
        if let Some(project) = thread.project_id.as_ref() {
            if let Some(page) = self.lists.project_threads.get(project)
                && let Some((index, row)) = updated_row(&page.data, thread, inserting)
            {
                let project = row.project_id.as_ref().unwrap().clone();
                apply_row_update(
                    Arc::make_mut(&mut self.lists.project_threads)
                        .get_mut(&project)
                        .unwrap(),
                    index,
                    row,
                );
            }
        } else if let Some(page) = &mut self.lists.threads
            && let Some((index, row)) = updated_row(&page.data, thread, inserting)
        {
            apply_row_update(page, index, row);
        }
    }
}

fn rename_index(rows: &[Thread], session: &SessionRef, revision: u64) -> Option<usize> {
    rows.iter()
        .position(|row| row.id.as_ref() == Some(session) && row.list_revision < revision)
}

fn apply_rename(
    page: &mut Arc<crate::models::ThreadList>,
    index: usize,
    name: Option<&str>,
    revision: u64,
) {
    let thread = &mut Arc::make_mut(page).data[index];
    thread.name = name.map(str::to_owned);
    thread.list_revision = revision;
}

fn apply_row_update(
    page: &mut Arc<crate::models::ThreadList>,
    index: Option<usize>,
    thread: Thread,
) {
    let page = Arc::make_mut(page);
    if let Some(index) = index {
        page.data[index] = thread;
    } else {
        page.data.push(thread);
    }
    let (data, more) = operations::page_rows(std::mem::take(&mut page.data), page.limit);
    page.data = data;
    page.has_more |= more;
}

fn updated_row(
    rows: &[Thread],
    mut thread: Thread,
    insert: bool,
) -> Option<(Option<usize>, Thread)> {
    let index = rows.iter().position(|row| row.id == thread.id);
    if index.is_none() && !insert
        || index.is_some_and(|index| rows[index].list_revision > thread.list_revision)
    {
        return None;
    }
    if let Some(old) = index.map(|index| &rows[index]) {
        // Rename notices own existing names; submission metadata may have been
        // read before a native automatic rename.
        thread.name = old
            .name
            .as_ref()
            .filter(|name| !name.trim().is_empty())
            .cloned()
            .or(thread.name);
        if old.cwd == thread.cwd {
            thread.git_branch = old.git_branch.clone().or(thread.git_branch);
            thread.has_changes = old.has_changes;
        }
    }
    if index.is_some_and(|index| rows[index] == thread) {
        return None;
    }
    Some((index, thread))
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
        .sessions
        .subscriptions
        .iter()
        .find(|(_, subscription)| **subscription == update.subscription_id)
    else {
        return (previous.clone(), Vec::new());
    };
    let mut next = previous.clone();
    let Some(current) = previous.sessions.conversations.get(id) else {
        return (next, Vec::new());
    };
    let thread = match update.change.apply(current) {
        Ok(thread) => thread,
        Err(_) => {
            Arc::make_mut(&mut next.sessions.subscriptions).remove(id);
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
    let active = crate::models::task_active(None, thread.status);
    Arc::make_mut(&mut next.sessions.conversations).insert(id.clone(), Arc::new(thread));
    reconcile_pending(&mut next, id);
    let mut effects = details;
    let panes: Vec<_> = next.panes.0.keys().cloned().collect();
    for pane_id in panes {
        let view_effects =
            update_pane_context(&mut next, &pane_id, current, id, &update.change, completed);
        effects.extend(
            view_effects
                .into_iter()
                .map(|effect| effect.in_pane(&pane_id)),
        );
    }
    if next.connection.connected
        && completed
        && !active
        && next
            .sessions
            .conversations
            .get(id)
            .is_none_or(|thread| thread.requests.is_empty())
        && !next.selected_in_any_pane(id)
    {
        Arc::make_mut(&mut next.sessions.subscriptions).remove(id);
    }
    if workspace_mutated(&update.change)
        && let Some(cwd) = &current.cwd
    {
        effects.extend(op::refresh_file_trees(&mut next, cwd));
    }
    (next, effects)
}

fn update_pane_context(
    snapshot: &mut Snapshot,
    pane_id: &str,
    before: &Thread,
    id: &crate::session::SessionRef,
    change: &crate::session::SessionChange,
    completed: bool,
) -> Vec<Effect> {
    use crate::session::SessionChange;
    let Some(pane) = snapshot.panes.get(pane_id).cloned() else {
        return Vec::new();
    };
    let thread = snapshot.sessions.conversations[id].clone();
    let workspace_changed = pane.navigation.thread_id.as_ref() == Some(id)
        && matches!(change, SessionChange::Workspace { .. })
        && thread.cwd.as_deref() != Some(pane.navigation.cwd.as_str());
    let refresh_workspace = (completed
        || before.preparation.is_some()
            && matches!(change, SessionChange::Preparation { preparation: None })
        || matches!(change, SessionChange::Item { .. }))
        && before.cwd.as_deref() == Some(&pane.navigation.cwd);
    if workspace_changed {
        let navigation = Navigation {
            cwd: thread.cwd.clone().unwrap_or_default(),
            ..pane.navigation.as_ref().clone()
        };
        snapshot.invalidate_pane(pane_id);
        navigate(snapshot, pane_id, navigation);
    }
    let mut effects = Vec::new();
    if fleet_contains(
        pane.observed_agents.as_ref(),
        id,
        &snapshot.sessions.conversations,
    ) && matches!(change, SessionChange::Item { item, .. }
            if matches!(item.body(), crate::models::ItemBody::Subagent { receivers, .. }
                if receivers.iter().any(|receiver| !snapshot.sessions.conversations.contains_key(receiver))))
    {
        effects.extend(op::refresh_agents(
            snapshot.connection.connected,
            pane.observed_agents.as_ref(),
        ));
    }
    if refresh_workspace || workspace_changed {
        effects.extend(op::review_workspace(snapshot, pane_id));
    }
    effects
}

// Match T3 ChatView.logic.ts deriveWorkspaceMutationId: completed shell/file
// tools and completed turns refresh file views; prose streaming does not.
fn workspace_mutated(change: &crate::session::SessionChange) -> bool {
    use crate::session::SessionChange;
    match change {
        SessionChange::Turn {
            completed: true, ..
        } => true,
        SessionChange::Item { item, .. } => {
            !matches!(
                item.status,
                crate::models::ItemStatus::Running | crate::models::ItemStatus::Unknown
            ) && matches!(
                item.body(),
                crate::models::ItemBody::FileChange { .. }
                    | crate::models::ItemBody::CommandExecution { .. }
            )
        }
        _ => false,
    }
}

impl Snapshot {
    pub(crate) fn accept_task_activity(
        &mut self,
        state: agent_protocol::live_activity::TaskActivityState,
    ) -> Vec<Effect> {
        if self
            .lists
            .task_activity
            .as_ref()
            .is_none_or(|current| current.revision < state.revision)
        {
            let unread = state
                .unread
                .iter()
                .map(|(id, _)| id.clone())
                .filter(|id| self.panes.primary().navigation.thread_id.as_ref() != Some(id))
                .collect();
            if self.lists.activity.unread != unread {
                Arc::make_mut(&mut self.lists.activity).unread = unread;
            }
            for (id, status) in &state.statuses {
                let active = match status {
                    crate::models::SessionStatus::Preparing
                    | crate::models::SessionStatus::Running => Some(true),
                    crate::models::SessionStatus::Idle => Some(false),
                    _ => None,
                };
                let current = self.lists.activity.active.get(id).copied();
                if current != active {
                    let activity = Arc::make_mut(&mut self.lists.activity);
                    match (current, active) {
                        (Some(_), Some(active)) => *activity.active.get_mut(id).unwrap() = active,
                        (None, Some(active)) => {
                            activity.active.insert(id.clone(), active);
                        }
                        (_, None) => {
                            activity.active.remove(id);
                        }
                    }
                }
            }
            if !state.statuses.is_empty()
                && (self
                    .lists
                    .threads
                    .as_ref()
                    .is_some_and(|page| !page.data.is_empty())
                    || self
                        .lists
                        .project_threads
                        .values()
                        .any(|page| !page.data.is_empty()))
            {
                let statuses: BTreeMap<_, _> = state
                    .statuses
                    .iter()
                    .map(|(id, status)| (id, *status))
                    .collect();
                if let Some(page) = &mut self.lists.threads
                    && let Some(first) = first_status_change(&page.data, &statuses)
                {
                    let page = Arc::make_mut(page);
                    page.data =
                        updated_status_rows(std::mem::take(&mut page.data), first, &statuses);
                }
                let updates: Vec<_> = self
                    .lists
                    .project_threads
                    .values()
                    .map(|page| first_status_change(&page.data, &statuses))
                    .collect();
                if updates.iter().any(Option::is_some) {
                    // Cloning this BTreeMap preserves its keys and iteration order.
                    for (page, index) in Arc::make_mut(&mut self.lists.project_threads)
                        .values_mut()
                        .zip(updates)
                    {
                        if let Some(index) = index {
                            let page = Arc::make_mut(page);
                            page.data = updated_status_rows(
                                std::mem::take(&mut page.data),
                                index,
                                &statuses,
                            );
                        }
                    }
                }
            }
            let previous = self
                .lists
                .task_activity
                .as_ref()
                .map(|state| state.unread.as_slice())
                .unwrap_or_default();
            let receipt = self
                .panes
                .primary()
                .navigation
                .thread_id
                .as_ref()
                .filter(|_| self.connection.connected)
                .and_then(|selected| {
                    state
                        .unread
                        .iter()
                        .find(|(id, _)| id == selected)
                        .filter(|completion| !previous.contains(completion))
                        .map(|(session, turn_id)| {
                            Effect::execute(op::MarkSessionRead {
                                session: session.clone(),
                                turn_id: turn_id.clone(),
                            })
                        })
                });
            self.lists.task_activity = Some(Arc::new(state));
            return receipt.into_iter().collect();
        }
        Vec::new()
    }
}

fn first_status_change(
    rows: &[Thread],
    statuses: &BTreeMap<&SessionRef, crate::models::SessionStatus>,
) -> Option<usize> {
    rows.iter().position(|thread| {
        thread
            .id
            .as_ref()
            .and_then(|id| statuses.get(id))
            .is_some_and(|status| thread.status != *status)
    })
}

fn updated_status_rows(
    mut rows: Vec<Thread>,
    index: usize,
    statuses: &BTreeMap<&SessionRef, crate::models::SessionStatus>,
) -> Vec<Thread> {
    // The prefix was already verified before the owner copied the page.
    for thread in &mut rows[index..] {
        if let Some(status) = thread.id.as_ref().and_then(|id| statuses.get(id)) {
            thread.status = *status;
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Notification;
    use crate::session::{ProviderKind, SessionRef};
    use crate::state::operations::Operation;
    use proptest::prelude::*;
    use serde_json::json;

    #[rstest::rstest]
    fn prepared_workspace_updates_only_its_open_panel_without_losing_new_draft_edits(
        #[values(true, false)] selected: bool,
        #[values("", "side")] pane_id: &str,
    ) {
        let id = SessionRef::new(ProviderKind::Codex, "admitted".into()).unwrap();
        let subscription = uuid::Uuid::new_v4();
        let draft_key = if selected {
            DraftKey::from(id.clone())
        } else {
            DraftKey::from("another")
        };
        let mut snapshot = Snapshot {
            connection: crate::state::Connection {
                connected: true,
                ..Default::default()
            },
            sessions: crate::state::Sessions {
                conversations: Arc::new(
                    [(
                        id.clone(),
                        Arc::new(Thread {
                            id: Some(id.clone()),
                            cwd: Some("/project".into()),
                            ..Default::default()
                        }),
                    )]
                    .into(),
                ),
                subscriptions: Arc::new([(id.clone(), subscription)].into()),
                ..Default::default()
            },
            input: crate::state::Input {
                drafts: Arc::new(
                    [(
                        draft_key.clone(),
                        Arc::new(Draft {
                            text: "typed while preparing".into(),
                            ..Default::default()
                        }),
                    )]
                    .into(),
                ),
                ..Default::default()
            },
            ..Default::default()
        };
        snapshot.open_pane(pane_id.into(), "/project".into());
        Arc::make_mut(snapshot.panes.get_mut(pane_id).unwrap()).navigation = Arc::new(Navigation {
            thread_id: selected.then_some(id.clone()),
            draft_key: draft_key.clone(),
            cwd: "/project".into(),
        });
        let file = op::OperationKey::File {
            pane: pane_id.into(),
        };
        Arc::make_mut(&mut snapshot.operations).insert(
            file.clone(),
            op::OperationState {
                generation: 1,
                phase: op::OperationPhase::Failed {
                    message: "old workspace".into(),
                },
            },
        );
        let (next, effects) = session_update(
            &snapshot,
            crate::session::SessionUpdate {
                subscription_id: subscription,
                change: crate::session::SessionChange::Workspace {
                    cwd: "/worktree".into(),
                },
            },
        );
        assert_eq!(
            next.sessions.conversations[&id].cwd.as_deref(),
            Some("/worktree")
        );
        assert_eq!(
            next.panes.get(pane_id).unwrap().navigation.cwd,
            if selected { "/worktree" } else { "/project" }
        );
        assert_eq!(
            next.operations.contains_key(&file),
            !selected,
            "only a pane whose workspace moved loses its old read error"
        );
        assert_eq!(next.input.drafts[&draft_key].text, "typed while preparing");
        assert_eq!(effects.len(), usize::from(selected));
    }

    #[test]
    fn a_preparing_task_reviews_no_changes_until_its_turn_starts() {
        use agent_protocol::session::{PreparationStatus, SessionPreparation};
        let id = SessionRef::new(ProviderKind::Codex, "admitted".into()).unwrap();
        let subscription = uuid::Uuid::new_v4();
        let snapshot = Snapshot {
            connection: crate::state::Connection {
                connected: true,
                ..Default::default()
            },
            sessions: crate::state::Sessions {
                conversations: Arc::new(
                    [(
                        id.clone(),
                        Arc::new(Thread {
                            id: Some(id.clone()),
                            cwd: Some("/project".into()),
                            preparation: Some(SessionPreparation {
                                submission: agent_protocol::operations::Submission {
                                    thread_id: id.clone(),
                                    client_user_message_id: "input".into(),
                                    input: Vec::new(),
                                    model: None,
                                    effort: None,
                                    service_tier: None,
                                    follow_up: Default::default(),
                                },
                                status: PreparationStatus::Preparing,
                                stage: agent_protocol::session::PreparationStage::Workspace,
                            }),
                            ..Default::default()
                        }),
                    )]
                    .into(),
                ),
                subscriptions: Arc::new([(id.clone(), subscription)].into()),
                ..Default::default()
            },
            panes: crate::state::PaneStates::with_primary(std::sync::Arc::new(
                crate::state::PaneState {
                    navigation: Arc::new(Navigation {
                        thread_id: Some(id.clone()),
                        draft_key: id.clone().into(),
                        cwd: "/project".into(),
                    }),
                    ..Default::default()
                },
            )),
            ..Default::default()
        };
        let update = |snapshot: &Snapshot, change| {
            session_update(
                snapshot,
                crate::session::SessionUpdate {
                    subscription_id: subscription,
                    change,
                },
            )
        };
        let mut reviewing = snapshot.clone();
        assert!(
            op::review_workspace(&mut reviewing, "").is_none(),
            "the source checkout's changes are not the preparing task's"
        );
        let (moved, effects) = update(
            &snapshot,
            crate::session::SessionChange::Workspace {
                cwd: "/worktree".into(),
            },
        );
        assert!(effects.is_empty());
        assert!(moved.panes.primary().workspace.review_cwd.is_none());
        let (started, effects) = update(
            &moved,
            crate::session::SessionChange::Preparation { preparation: None },
        );
        assert_eq!(
            effects.len(),
            1,
            "completion reviews even if the provider already sent its items"
        );
        let (_, repeated) = update(
            &started,
            crate::session::SessionChange::Preparation { preparation: None },
        );
        assert!(
            repeated.is_empty(),
            "replayed completion does not refresh changes again"
        );
        let item = serde_json::from_value(json!({
            "id":"user","status":"completed","clientInputId":"input",
            "body":{"inline":{"body":{"userMessage":{"text":"start","content":[]}}}}
        }))
        .unwrap();
        let (_, effects) = update(
            &started,
            crate::session::SessionChange::Item {
                turn_id: "turn".into(),
                item: Arc::new(item),
            },
        );
        assert_eq!(effects.len(), 1, "the started task reviews its worktree");
    }

    #[rstest::rstest]
    fn known_child_activity_updates_rows_without_refetching(
        #[values(true, false)] active: bool,
        #[values(true, false)] observing: bool,
    ) {
        let parent = SessionRef::new(ProviderKind::Codex, "parent".into()).unwrap();
        let child = SessionRef::new(ProviderKind::Codex, "child".into()).unwrap();
        let snapshot = Snapshot {
            connection: crate::state::Connection {
                connected: true,
                ..Default::default()
            },
            sessions: crate::state::Sessions {
                conversations: Arc::new(
                    [(
                        child.clone(),
                        Arc::new(crate::models::Thread {
                            id: Some(child.clone()),
                            parent_id: Some(parent.clone()),
                            name: Some("Review".into()),
                            ..Default::default()
                        }),
                    )]
                    .into(),
                ),
                ..Default::default()
            },
            panes: crate::state::PaneStates::with_primary(std::sync::Arc::new(
                crate::state::PaneState {
                    observed_agents: observing.then_some(parent.clone()),
                    navigation: Arc::new(Navigation {
                        thread_id: Some(parent.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )),
            ..Default::default()
        };
        let (next, effects) = notification(
            &snapshot,
            crate::protocol::Notification::Activity {
                session: child.clone(),
                active,
            },
        );
        assert!(effects.is_empty());
        assert_eq!(next.lists.activity.active[&child], active);
        assert_eq!(next.agent_panel(next.panes.primary())[0].active, active);
        assert_eq!(
            next.panes.primary().navigation,
            snapshot.panes.primary().navigation
        );
    }

    #[rstest::rstest]
    fn shared_unread_reconciles_both_clients_and_acknowledges_each_visible_completion_once(
        #[values(ProviderKind::Codex, ProviderKind::Claude)] provider: ProviderKind,
    ) {
        let id = SessionRef::new(provider, "task".into()).unwrap();
        let activity = |revision, turn: Option<&str>| Notification::TaskActivity {
            state: agent_protocol::live_activity::TaskActivityState {
                attention: Vec::new(),
                revision,
                waiting: Vec::new(),
                display: agent_protocol::live_activity::TaskActivitySummary::default().display(),
                statuses: Vec::new(),
                background: Vec::new(),
                unread: turn
                    .map(|turn| (id.clone(), turn.into()))
                    .into_iter()
                    .collect(),
            },
        };
        let initial = Snapshot {
            connection: crate::state::Connection {
                connected: true,
                ..Default::default()
            },
            catalog: crate::state::Catalog {
                composer_catalogs: std::sync::Arc::new(
                    (Some(Arc::new(
                        agent_protocol::composer::ComposerCatalog::default(),
                    )))
                    .into_iter()
                    .map(|catalog| (catalog.cwd.clone(), catalog))
                    .collect(),
                ),
                ..Default::default()
            },
            ..Default::default()
        };
        // Task-list unread state follows the primary selection, even when
        // another pane keeps the conversation open and subscribed.
        let mut side_only = initial.clone();
        side_only.open_pane("side".into(), String::new());
        Arc::make_mut(side_only.panes.get_mut("side").unwrap()).navigation = Arc::new(Navigation {
            thread_id: Some(id.clone()),
            draft_key: id.clone().into(),
            cwd: String::new(),
        });
        let (side_only, effects) = reduce(
            &side_only,
            "",
            Event::Notification(activity(1, Some("first"))),
        );
        assert!(side_only.lists.activity.unread.contains(&id));
        assert!(
            effects.is_empty(),
            "a side selection must not send a primary read receipt"
        );
        let (phone, effects) = reduce(
            &initial,
            "",
            Event::Notification(activity(1, Some("first"))),
        );
        assert!(effects.is_empty());
        assert!(phone.lists.activity.unread.contains(&id));
        let (desktop, _) = reduce(
            &initial,
            "",
            Event::Notification(activity(1, Some("first"))),
        );
        assert_eq!(desktop.lists.activity.unread, phone.lists.activity.unread);

        let (phone, effects) = reduce(
            &phone,
            "",
            Event::Intent(Intent::ReadThread(op::ReadThread::open(id.clone()))),
        );
        assert_eq!(
            effects.len(),
            2,
            "opening the conversation reads history and sends a receipt"
        );
        assert!(phone.lists.activity.unread.is_empty());
        let (phone, effects) = reduce(&phone, "", Event::Notification(activity(2, Some("first"))));
        assert!(
            effects.is_empty(),
            "unrelated Host revisions must not repeat a receipt"
        );
        let (phone, effects) = reduce(&phone, "", Event::Notification(activity(3, Some("second"))));
        assert_eq!(
            effects.len(),
            1,
            "a new visible completion needs its own receipt"
        );
        assert!(phone.lists.activity.unread.is_empty());
        let (phone, effects) = reduce(&phone, "", Event::Notification(activity(1, None)));
        assert!(effects.is_empty());
        assert_eq!(phone.lists.task_activity.as_ref().unwrap().revision, 3);

        let (desktop, effects) = reduce(&desktop, "", Event::Notification(activity(4, None)));
        assert!(effects.is_empty());
        assert!(desktop.lists.activity.unread.is_empty());
        let disconnected = reduce(&phone, "", Event::Disconnected).0;
        let reconnected = reduce(&disconnected, "", Event::Connected).0;
        let mut restored = reconnected;
        let effects = op::ReadTaskActivity {}.apply(
            &mut restored,
            "",
            match activity(0, Some("second")) {
                Notification::TaskActivity { state } => state,
                _ => unreachable!(),
            },
        );
        assert_eq!(
            effects.len(),
            1,
            "an unconfirmed receipt is retried after reconnect"
        );
    }

    fn host_unread(state: &mut Snapshot, id: &SessionRef, unread: bool) {
        let _ = state.accept_task_activity(agent_protocol::live_activity::TaskActivityState {
            attention: Vec::new(),
            waiting: Vec::new(),
            revision: state
                .lists
                .task_activity
                .as_ref()
                .map_or(1, |state| state.revision + 1),
            display: agent_protocol::live_activity::TaskActivitySummary::default().display(),
            statuses: Vec::new(),
            background: Vec::new(),
            unread: if unread {
                vec![(id.clone(), "turn".into())]
            } else {
                Vec::new()
            },
        });
    }

    fn snapshot(provider: ProviderKind) -> Snapshot {
        let parent = SessionRef::new(provider, "parent".into()).unwrap();
        let subscription = uuid::Uuid::new_v4();
        Snapshot {
            connection: crate::state::Connection {
                connected: true,
                ..Default::default()
            },
            lists: crate::state::Lists {
                threads: Some(Arc::new(serde_json::from_value(json!({
                    "data":[{"id":parent,"name":"Task","cwd":"/fixture","status":"idle","hasChanges":true}],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,})).unwrap())),
                ..Default::default()
            },
            sessions: crate::state::Sessions {
                subscriptions: Arc::new([(parent.clone(), subscription)].into()),
                conversations: Arc::new([(parent.clone(), Arc::new(crate::models::Thread {
                    id: Some(parent.clone()),
                    turns: Some(vec![Arc::new(crate::models::Turn { id:"turn".into(), status:crate::models::TurnStatus::Running, items:Some(vec![]), ..Default::default() })]),
                    ..Default::default()
                }))].into()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn replacing_history_drops_removed_turns_pages_and_late_reads_without_losing_the_draft() {
        use crate::state::operations::Operation;
        for subscribed in [true, false] {
            let mut state = snapshot(ProviderKind::Codex);
            let id = SessionRef::new(ProviderKind::Codex, "parent".into()).unwrap();
            if !subscribed {
                Arc::make_mut(&mut state.sessions.subscriptions).clear();
            }
            let thread = Arc::make_mut(
                Arc::make_mut(&mut state.sessions.conversations)
                    .get_mut(&id)
                    .unwrap(),
            );
            thread.transcript_version = Some(1);
            thread.history_cursor = Some("10".into());
            thread.history_has_more = Some(true);
            let before = thread.clone();
            let mut replacement = before.clone();
            replacement.turns = Some(Vec::new());
            replacement.transcript_version = Some(2);
            replacement.history_cursor = None;
            replacement.history_has_more = Some(false);
            Arc::make_mut(&mut state.input.drafts).insert(
                id.clone().into(),
                Arc::new(crate::state::Draft {
                    text: "keep this draft".into(),
                    ..Default::default()
                }),
            );
            let key = op::OperationKey::History {
                session: id.clone(),
            };
            Arc::make_mut(&mut state.operations).insert(
                key.clone(),
                op::OperationState {
                    generation: 7,
                    phase: op::OperationPhase::Running,
                },
            );
            let (mut next, effects) = notification(
                &state,
                Notification::SessionHistoryReplaced {
                    thread: replacement.into(),
                },
            );
            assert_eq!(effects.len(), 1, "refresh the authoritative subscription");
            assert!(!next.operations.contains_key(&key));
            assert_eq!(next.sessions.subscriptions, state.sessions.subscriptions);
            assert_eq!(next.input.drafts, state.input.drafts);
            let late = crate::session::OpenedSession {
                session: id.clone(),
                subscription_id: uuid::Uuid::new_v4(),
                unchanged: false,
                response: crate::models::ThreadResponse {
                    thread: before,
                    ..Default::default()
                },
            };
            op::ReadThread::new(id.clone()).stale(&mut next, "", late);
            let held = &next.sessions.conversations[&id];
            assert!(held.turns.as_ref().unwrap().is_empty());
            assert_eq!(held.history_cursor, None);
            assert_eq!(held.history_has_more, Some(false));
            assert_eq!(held.transcript_version, Some(2));
        }
    }

    #[rstest::rstest]
    fn unknown_activity_does_not_repeat_discovery_reads(
        #[values(true, false)] active: bool,
        #[values(true, false)] observing: bool,
    ) {
        let mut state = snapshot(ProviderKind::Codex);
        let parent = state.lists.threads.as_ref().unwrap().data[0]
            .id
            .clone()
            .unwrap();
        Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).navigation).thread_id =
            Some(parent.clone());
        Arc::make_mut(state.panes.primary_mut()).observed_agents = observing.then_some(parent);
        let unknown = SessionRef::new(ProviderKind::Codex, "unseen".into()).unwrap();
        let (discovered, effects) = notification(
            &state,
            Notification::Activity {
                session: unknown.clone(),
                active,
            },
        );
        assert_eq!(effects.len(), usize::from(observing));
        let (repeated, effects) = notification(
            &discovered,
            Notification::Activity {
                session: unknown,
                active,
            },
        );
        assert!(effects.is_empty());
        assert!(Arc::ptr_eq(
            &discovered.lists.activity,
            &repeated.lists.activity
        ));
        assert_eq!(
            repeated.panes.primary().navigation,
            state.panes.primary().navigation
        );
    }

    #[test]
    fn unrelated_fleets_do_not_trigger_reads_of_the_visible_fleet() {
        let mut state = snapshot(ProviderKind::Codex);
        let parent = state.lists.threads.as_ref().unwrap().data[0]
            .id
            .clone()
            .unwrap();
        Arc::make_mut(state.panes.primary_mut()).observed_agents = Some(parent.clone());
        Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).navigation).thread_id =
            Some(parent);
        let session = |id: &str| SessionRef::new(ProviderKind::Codex, id.into()).unwrap();
        for (id, parent) in [
            ("child", "parent"),
            ("grandchild", "child"),
            ("unrelated", "other"),
            ("cycle-a", "cycle-b"),
            ("cycle-b", "cycle-a"),
        ] {
            let id = session(id);
            Arc::make_mut(&mut state.sessions.subscriptions)
                .insert(id.clone(), uuid::Uuid::new_v4());
            Arc::make_mut(&mut state.sessions.conversations).insert(
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
        Arc::make_mut(&mut state.sessions.subscriptions)
            .insert(cross_provider.clone(), uuid::Uuid::new_v4());
        let mut cross_thread = state.sessions.conversations[&session("child")]
            .as_ref()
            .clone();
        cross_thread.id = Some(cross_provider.clone());
        Arc::make_mut(&mut state.sessions.conversations)
            .insert(cross_provider.clone(), Arc::new(cross_thread));
        for (id, expected) in [
            ("child", 1),
            ("grandchild", 1),
            ("unrelated", 0),
            ("cycle-a", 0),
        ]
        .into_iter()
        .map(|(id, expected)| (session(id), expected))
        // After a handoff the public parent retains its original provider,
        // while a native child belongs to the active provider.
        .chain([(cross_provider, 1)])
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
                    subscription_id: state.sessions.subscriptions[&id],
                    change: crate::session::SessionChange::Item {
                        turn_id: "turn".into(),
                        item: Arc::new(item),
                    },
                },
            );
            assert_eq!(effects.len(), expected, "spawn {id}");
            assert_eq!(
                next.panes.primary().navigation,
                state.panes.primary().navigation
            );
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
            state.connection.connected = connected;
            Arc::make_mut(state.lists.threads.as_mut().unwrap()).data[0].cwd = Some("/fixture".into());
            let id = state.lists.threads.as_ref().unwrap().data[0].id.clone().unwrap();
            if selected {
                Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).navigation).thread_id = Some(id.clone());
            }
            Arc::make_mut(&mut state.lists.activity).unread.insert(id.clone());
            let cached_list = state.lists.threads.clone().unwrap();
            for _ in 0..2 {
                let (running, effects) = notification(&state, Notification::Activity {
                    session: id.clone(), active: true,
                });
                prop_assert!(effects.is_empty());
                prop_assert!(running.thread_list().unwrap().threads[0].active);
                let mut running = running;
                host_unread(&mut running, &id, false);
                prop_assert!(!running.thread_list().unwrap().threads[0].unread);
                state = running;
                for _ in 0..repetitions {
                    let (repeated, effects) = notification(&state, Notification::Activity {
                        session: id.clone(), active: true,
                    });
                    prop_assert!(effects.is_empty());
                    prop_assert!(Arc::ptr_eq(&state.lists.activity, &repeated.lists.activity));
                    state = repeated;
                }
                let (idle, effects) = notification(&state, Notification::Activity {
                    session: id.clone(), active: false,
                });
                prop_assert_eq!(effects.len(), usize::from(connected));
                state = idle;
                for _ in 0..repetitions {
                    let (ended, effects) = notification(&state, Notification::Activity {
                        session: id.clone(), active: false,
                    });
                    prop_assert!(effects.is_empty());
                    let mut ended = ended;
                    host_unread(&mut ended, &id, finished);
                    let row = &ended.thread_list().unwrap().threads[0];
                    prop_assert!(!row.active);
                    prop_assert_eq!(row.unread, finished && !selected);
                    prop_assert_eq!(row.title.as_str(), "Task");
                    prop_assert_eq!(row.has_changes, Some(true));
                    prop_assert_eq!(&state.panes.primary().navigation, &ended.panes.primary().navigation);
                    state = ended;
                }
                prop_assert!(Arc::ptr_eq(state.lists.threads.as_ref().unwrap(), &cached_list));
                let (repeated, effects) = notification(&state, Notification::Activity {
                    session: id.clone(), active: false,
                });
                prop_assert!(effects.is_empty());
                prop_assert!(Arc::ptr_eq(&state.lists.activity, &repeated.lists.activity));
            }
        }
    }

    #[rstest::rstest]
    #[case::codex(ProviderKind::Codex)]
    #[case::claude(ProviderKind::Claude)]
    fn streamed_commands_and_known_agents_do_not_reload_titles(#[case] provider: ProviderKind) {
        let mut state = snapshot(provider);
        Arc::make_mut(state.lists.threads.as_mut().unwrap()).data[0].cwd = Some("/fixture".into());
        let id = state.lists.threads.as_ref().unwrap().data[0]
            .id
            .clone()
            .unwrap();
        let subscription = state.sessions.subscriptions[&id];
        let child = SessionRef::new(provider, "child".into()).unwrap();
        Arc::make_mut(state.panes.primary_mut()).observed_agents = Some(id.clone());
        Arc::make_mut(&mut state.sessions.conversations).insert(
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
        assert!(!ended.sessions.subscriptions.contains_key(&id));
        let (ended, effects) = notification(
            &ended,
            Notification::Activity {
                session: id.clone(),
                active: false,
            },
        );
        assert_eq!(
            effects.len(),
            1,
            "completion still refreshes Git and metadata"
        );
        let mut ended = ended;
        host_unread(&mut ended, &id, true);
        let row = &ended.thread_list().unwrap().threads[0];
        assert!(!row.active);
        assert!(row.unread);
    }

    #[test]
    fn user_messages_and_renames_do_not_read_lists() {
        let state = snapshot(ProviderKind::Codex);
        let id = state.lists.threads.as_ref().unwrap().data[0]
            .id
            .clone()
            .unwrap();
        let item = serde_json::from_value(json!({
            "id":"user","status":"completed","clientInputId":null,
            "body":{"inline":{"body":{"userMessage":{"text":"New task title","content":[]}}}}
        }))
        .unwrap();
        let (_, effects) = session_update(
            &state,
            crate::session::SessionUpdate {
                subscription_id: state.sessions.subscriptions[&id],
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
        assert_eq!(
            next.sessions.conversations[&id].name.as_deref(),
            Some("Renamed")
        );
        assert_eq!(
            next.lists.threads.as_ref().unwrap().data[0].name.as_deref(),
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
        assert!(cleared.sessions.conversations[&id].name.is_none());
        assert!(
            cleared.lists.threads.as_ref().unwrap().data[0]
                .name
                .is_none()
        );
    }

    proptest! {
        #[test]
        fn metadata_notices_preserve_closed_page_quota_and_newer_names(limit in prop_oneof![Just(5u32), Just(15u32), Just(25u32)]) {
            let mut state = snapshot(ProviderKind::Codex);
            let mut page = state.lists.threads.as_ref().unwrap().as_ref().clone();
            page.limit = limit;
            page.data = (0..limit).map(|index| Thread {
                id: Some(SessionRef::new(ProviderKind::Codex, format!("task-{index}")).unwrap()),
                project_id: crate::models::ProjectMembership::Assigned("closed".into()),
                name: Some(format!("Task {index}")),
                updated_at: Some(f64::from(index)),
                ..Default::default()
            }).collect();
            state.lists.project_threads = Arc::new([("closed".into(), Arc::new(page))].into());
            let id = state.lists.project_threads["closed"].data[0].id.clone().unwrap();
            let (renamed, effects) = notification(&state, Notification::SessionRenamed { session: id.clone(), name: Some("New title".into()), revision: 2 });
            prop_assert!(effects.is_empty());
            let (next, effects) = notification(&renamed, Notification::SessionUpdated {
                thread: Thread { id: Some(id.clone()), project_id: crate::models::ProjectMembership::Assigned("closed".into()), name: Some("Old native name".into()), updated_at: Some(999.0), list_revision: 3, ..Default::default() }.into(), project: None,
            });
            prop_assert!(effects.is_empty());
            prop_assert!(next.lists.expanded_projects.is_empty());
            prop_assert_eq!(next.lists.project_threads["closed"].limit, limit);
            prop_assert_eq!(next.lists.project_threads["closed"].data.len(), limit as usize);
            prop_assert!(!next.lists.project_threads["closed"].has_more, "updating an existing row cannot invent a lookahead row");
            prop_assert_eq!(next.lists.project_threads["closed"].data[0].name.as_deref(), Some("New title"));
            let (stale, effects) = notification(&next, Notification::SessionRenamed { session: id, name: Some("Stale".into()), revision: 1 });
            prop_assert!(effects.is_empty());
            prop_assert!(Arc::ptr_eq(&stale.lists.project_threads, &next.lists.project_threads));
            let (stale, effects) = notification(&next, Notification::SessionUpdated { thread: state.lists.project_threads["closed"].data[0].clone().into(), project: None });
            prop_assert!(effects.is_empty());
            prop_assert!(Arc::ptr_eq(&stale.lists.project_threads, &next.lists.project_threads));
            let (added, effects) = notification(&next, Notification::SessionUpdated {
                thread: Thread { id: Some(SessionRef::new(ProviderKind::Codex, "new".into()).unwrap()), project_id: crate::models::ProjectMembership::Assigned("closed".into()), name: Some("Created task".into()), updated_at: Some(1000.0), list_revision: 4, ..Default::default() }.into(), project: None,
            });
            prop_assert!(effects.is_empty());
            prop_assert_eq!(added.lists.project_threads["closed"].data.len(), limit as usize);
            prop_assert!(added.lists.project_threads["closed"].has_more);
            prop_assert_eq!(added.lists.project_threads["closed"].data[0].name.as_deref(), Some("Created task"));
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
        assert_eq!(
            next.lists.threads.as_ref().unwrap().projects[0].id,
            "unread"
        );
        assert!(!next.lists.project_threads.contains_key("unread"));
        let (_, effects) = reduce_intent(
            &next,
            "",
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
        let parent = snapshot.lists.threads.as_ref().unwrap().data[0]
            .id
            .clone()
            .unwrap();
        let child = SessionRef::new(ProviderKind::Codex, "child".into()).unwrap();
        let subscription = snapshot.sessions.subscriptions[&parent];
        for session in [parent.clone(), child.clone()] {
            let (next, effects) = notification(
                &snapshot,
                crate::protocol::Notification::Activity {
                    session: session.clone(),
                    active: true,
                },
            );
            assert!(effects.is_empty(), "activity alone must not read lists");
            assert!(next.lists.activity.active[&session]);
            assert_eq!(
                next.panes.primary().navigation,
                snapshot.panes.primary().navigation
            );
        }
        let item = serde_json::from_value(serde_json::json!({
            "id":"spawn","status":"running","clientInputId":null,
            "body":{"inline":{"body":{"subagent":{"tool":"spawnAgent","prompt":null,"model":null,"effort":null,"sender":parent,"receivers":[child],"states":[],"agentId":null,"result":null}}}}
        })).unwrap();
        Arc::make_mut(snapshot.panes.primary_mut()).observed_agents = Some(parent);
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
        assert_eq!(
            next.panes.primary().navigation,
            snapshot.panes.primary().navigation
        );
    }
    #[test]
    fn completion_decorations_keep_the_first_listed_directory_and_skip_child_sessions() {
        let mut state = snapshot(ProviderKind::Codex);
        let first = Arc::make_mut(state.lists.threads.as_mut().unwrap())
            .data
            .first_mut()
            .unwrap();
        let id = first.id.clone().unwrap();
        first.cwd = Some("/first".into());
        first.git_branch = Some("first-branch".into());
        let mut duplicate = first.clone();
        duplicate.cwd = Some("/later".into());
        duplicate.git_branch = Some("later-branch".into());
        let root = Arc::make_mut(state.lists.threads.as_mut().unwrap());
        root.data.push(duplicate.clone());
        let mut project = root.clone();
        project.data = vec![duplicate];
        Arc::make_mut(&mut state.lists.project_threads).insert("project".into(), Arc::new(project));
        Arc::make_mut(&mut state.lists.activity)
            .active
            .insert(id.clone(), true);
        let (idle, effects) = notification(
            &state,
            Notification::Activity {
                session: id.clone(),
                active: false,
            },
        );
        assert_eq!(idle.lists.activity.active.get(&id), Some(&false));
        let expected = vec![Effect::execute(op::ReadListDecorations {
            scope: agent_protocol::operations::ListDecorationScope::Task {
                session: id.clone(),
            },
            projects: vec![],
            threads: vec![(id.clone(), "/first".into(), Some("first-branch".into()))],
        })];
        assert_eq!(format!("{effects:?}"), format!("{expected:?}"));
        Arc::make_mut(state.lists.threads.as_mut().unwrap()).data[0].cwd = None;
        assert!(
            notification(
                &state,
                Notification::Activity {
                    session: id.clone(),
                    active: false
                }
            )
            .1
            .is_empty()
        );
        Arc::make_mut(state.lists.threads.as_mut().unwrap()).data[0].cwd = Some("/first".into());
        Arc::make_mut(
            Arc::make_mut(&mut state.sessions.conversations)
                .get_mut(&id)
                .unwrap(),
        )
        .parent_id = Some(SessionRef::new(ProviderKind::Codex, "owner".into()).unwrap());
        assert!(
            notification(
                &state,
                Notification::Activity {
                    session: id.clone(),
                    active: false
                }
            )
            .1
            .is_empty()
        );
    }
    #[test]
    fn active_status_changes_preserve_other_providers_and_previous_snapshots() {
        use crate::models::SessionStatus;
        use agent_protocol::live_activity::{TaskActivityState, TaskActivitySummary};
        let id = SessionRef::new(ProviderKind::Codex, "same".into()).unwrap();
        let other = SessionRef::new(ProviderKind::Claude, "same".into()).unwrap();
        for current in [None, Some(false), Some(true)] {
            for status in [
                SessionStatus::Unknown,
                SessionStatus::Idle,
                SessionStatus::Preparing,
                SessionStatus::Running,
            ] {
                let mut state = Snapshot::default();
                let activity = Arc::make_mut(&mut state.lists.activity);
                activity.active.insert(other.clone(), true);
                if let Some(current) = current {
                    activity.active.insert(id.clone(), current);
                }
                let (next, effects) = notification(
                    &state,
                    Notification::TaskActivity {
                        state: TaskActivityState {
                            attention: Vec::new(),
                            revision: 1,
                            waiting: vec![],
                            background: Vec::new(),
                            unread: vec![],
                            display: TaskActivitySummary::default().display(),
                            statuses: vec![(id.clone(), status)],
                        },
                    },
                );
                let expected = match status {
                    SessionStatus::Preparing | SessionStatus::Running => Some(true),
                    SessionStatus::Idle => Some(false),
                    _ => None,
                };
                assert!(effects.is_empty());
                assert_eq!(next.lists.activity.active.get(&id).copied(), expected);
                assert!(next.lists.activity.active[&other]);
                assert_eq!(state.lists.activity.active.get(&id).copied(), current);
                assert_eq!(
                    Arc::ptr_eq(&state.lists.activity, &next.lists.activity),
                    current == expected
                );
                assert_eq!(
                    next.lists.task_activity.as_ref().unwrap().statuses,
                    [(id.clone(), status)]
                );
            }
        }
        let mut state = Snapshot::default();
        Arc::make_mut(&mut state.lists.activity)
            .active
            .insert(id.clone(), false);
        let (next, effects) = notification(
            &state,
            Notification::TaskActivity {
                state: TaskActivityState {
                    attention: Vec::new(),
                    revision: 1,
                    waiting: vec![],
                    background: Vec::new(),
                    unread: vec![],
                    display: TaskActivitySummary::default().display(),
                    statuses: vec![
                        (id.clone(), SessionStatus::Running),
                        (id.clone(), SessionStatus::Idle),
                    ],
                },
            },
        );
        assert!(effects.is_empty());
        assert_eq!(next.lists.activity.active, state.lists.activity.active);
        assert!(!next.lists.activity.active[&id]);
    }

    #[test]
    fn activity_statuses_keep_page_metadata_last_duplicate_and_provider_identity() {
        use crate::models::SessionStatus;
        use agent_protocol::live_activity::{TaskActivityState, TaskActivitySummary};
        let codex = SessionRef::new(ProviderKind::Codex, "same".into()).unwrap();
        let claude = SessionRef::new(ProviderKind::Claude, "same".into()).unwrap();
        let last = SessionRef::new(ProviderKind::Codex, "last".into()).unwrap();
        for status in [
            SessionStatus::Unknown,
            SessionStatus::Preparing,
            SessionStatus::Running,
            SessionStatus::Idle,
        ] {
            let root=Arc::new(serde_json::from_value::<ThreadList>(json!({
                "revision":99,"limit":3,"hasMore":true,"hasMoreProjects":true,
                "projects":[{"id":"project","name":"Project","roots":[]}],"projectPages":{},
                "data":[
                    {"name":"No identity","status":"unknown"},
                    {"id":codex,"name":"Kept prefix","status":"idle","preview":"prefix","cwd":"/prefix","gitBranch":"branch","updatedAt":1},
                    {"id":claude,"name":"First change","status":"running","preview":"middle","cwd":"/middle","updatedAt":2},
                    {"id":last,"name":"Suffix change","status":"idle","preview":"suffix","cwd":"/suffix","updatedAt":3}
                ]
            })).unwrap());
            let mut unchanged = root.as_ref().clone();
            unchanged.data.truncate(2);
            let unchanged = Arc::new(unchanged);
            let state = Snapshot {
                lists: crate::state::Lists {
                    threads: Some(root.clone()),
                    project_threads: Arc::new(
                        [
                            ("matching".into(), root.clone()),
                            ("z-matching".into(), root.clone()),
                            ("a-unchanged".into(), unchanged.clone()),
                        ]
                        .into(),
                    ),
                    ..Default::default()
                },
                ..Default::default()
            };
            let incoming = TaskActivityState {
                attention: Vec::new(),
                revision: 1,
                waiting: vec![],
                display: TaskActivitySummary::default().display(),
                background: Vec::new(),
                unread: vec![],
                statuses: vec![
                    (codex.clone(), SessionStatus::Running),
                    (claude.clone(), SessionStatus::Idle),
                    (last.clone(), status),
                    (codex.clone(), SessionStatus::Idle),
                ],
            };
            let (next, effects) = notification(
                &state,
                Notification::TaskActivity {
                    state: incoming.clone(),
                },
            );
            assert!(effects.is_empty());
            let mut expected = root.as_ref().clone();
            expected.data[2].status = SessionStatus::Idle;
            expected.data[3].status = status;
            assert_eq!(next.lists.threads.as_ref().unwrap().as_ref(), &expected);
            assert_eq!(next.lists.project_threads["matching"].as_ref(), &expected);
            assert_eq!(next.lists.project_threads["z-matching"].as_ref(), &expected);
            let mut root_only = state.clone();
            root_only.lists.project_threads = Arc::default();
            let (root_only, effects) = notification(
                &root_only,
                Notification::TaskActivity {
                    state: incoming.clone(),
                },
            );
            assert!(effects.is_empty());
            assert_eq!(
                root_only.lists.threads.as_ref().unwrap().as_ref(),
                &expected
            );
            assert!(root_only.lists.project_threads.is_empty());
            let mut project_only = state.clone();
            project_only.lists.threads = None;
            let (project_only, effects) = notification(
                &project_only,
                Notification::TaskActivity {
                    state: incoming.clone(),
                },
            );
            assert!(effects.is_empty());
            assert!(project_only.lists.threads.is_none());
            assert_eq!(
                project_only.lists.project_threads["matching"].as_ref(),
                &expected
            );
            assert_eq!(
                project_only.lists.project_threads["z-matching"].as_ref(),
                &expected
            );
            assert!(Arc::ptr_eq(
                &next.lists.project_threads["a-unchanged"],
                &unchanged
            ));
            assert_eq!(root.data[2].status, SessionStatus::Running);
            assert!(!next.lists.activity.active[&codex]);
            assert!(!next.lists.activity.active[&claude]);
            assert_eq!(
                next.lists.activity.active.get(&last).copied(),
                match status {
                    SessionStatus::Preparing | SessionStatus::Running => Some(true),
                    SessionStatus::Idle => Some(false),
                    _ => None,
                }
            );
            let (same, effects) = notification(
                &next,
                Notification::TaskActivity {
                    state: TaskActivityState {
                        attention: Vec::new(),
                        revision: 2,
                        ..incoming
                    },
                },
            );
            assert!(effects.is_empty());
            assert!(Arc::ptr_eq(
                same.lists.threads.as_ref().unwrap(),
                next.lists.threads.as_ref().unwrap()
            ));
            assert!(Arc::ptr_eq(
                &same.lists.project_threads,
                &next.lists.project_threads
            ));
            let empty = TaskActivityState {
                attention: Vec::new(),
                revision: 3,
                waiting: vec![],
                display: TaskActivitySummary {
                    running: 2,
                    ..Default::default()
                }
                .display(),
                background: Vec::new(),
                unread: vec![],
                statuses: vec![],
            };
            let (empty, effects) = notification(&same, Notification::TaskActivity { state: empty });
            assert!(effects.is_empty());
            assert!(Arc::ptr_eq(
                empty.lists.threads.as_ref().unwrap(),
                same.lists.threads.as_ref().unwrap()
            ));
            assert!(Arc::ptr_eq(
                &empty.lists.project_threads,
                &same.lists.project_threads
            ));
            assert!(Arc::ptr_eq(&empty.lists.activity, &same.lists.activity));
            assert_eq!(empty.lists.task_activity.as_ref().unwrap().revision, 3);
            assert_eq!(
                empty.task_activity_display().unwrap(),
                TaskActivitySummary {
                    running: 2,
                    ..Default::default()
                }
                .display()
            );
        }
    }

    #[test]
    fn summary_insert_search_keeps_trimmed_unicode_matching_and_project_routes() {
        for (search, name, matches) in [
            ("", None, true),
            (" \t\n", Some("Anything"), true),
            ("\u{a0}", None, true),
            (" mixed ", Some("MiXeD title"), true),
            ("É", Some("CAFÉ"), true),
            ("Σ", Some("ΟΣ"), false),
            ("STRASSE", Some("Straße"), false),
            ("missing", Some("Different"), false),
            ("name", None, false),
        ] {
            for provider in [ProviderKind::Codex, ProviderKind::Claude] {
                for project_id in [None, Some("project")] {
                    let id = SessionRef::new(provider, "inserted".into()).unwrap();
                    let mut state = snapshot(provider);
                    state.sessions.conversations = Arc::default();
                    Arc::make_mut(&mut state.lists.list_query).search_term = search.into();
                    Arc::make_mut(state.lists.threads.as_mut().unwrap())
                        .data
                        .clear();
                    if project_id.is_some() {
                        state.lists.project_threads = Arc::new(
                            [("project".into(), state.lists.threads.clone().unwrap())].into(),
                        );
                    }
                    let incoming = Thread {
                        id: Some(id.clone()),
                        name: name.map(str::to_owned),
                        preview: Some("retained preview".into()),
                        project_id: project_id
                            .map_or(crate::models::ProjectMembership::Unassigned {}, |id| {
                                crate::models::ProjectMembership::Assigned(id.into())
                            }),
                        list_revision: 1,
                        ..Default::default()
                    };
                    let (next, effects) = notification(
                        &state,
                        Notification::SessionUpdated {
                            thread: Box::new(incoming.clone()),
                            project: None,
                        },
                    );
                    assert!(effects.is_empty());
                    let page = match project_id {
                        Some(id) => &next.lists.project_threads[id],
                        None => next.lists.threads.as_ref().unwrap(),
                    };
                    assert_eq!(
                        page.data,
                        if matches { vec![incoming] } else { vec![] },
                        "search={search:?}, name={name:?}, provider={provider:?}, project={project_id:?}"
                    );
                    assert!(state.lists.threads.as_ref().unwrap().data.is_empty());
                    if project_id.is_some() {
                        assert!(Arc::ptr_eq(
                            next.lists.threads.as_ref().unwrap(),
                            state.lists.threads.as_ref().unwrap(),
                        ));
                    }
                }
            }
        }
    }

    #[test]
    fn summary_rows_preserve_shared_snapshots_metadata_sorting_and_page_limits() {
        let mut state = snapshot(ProviderKind::Codex);
        state.sessions.conversations = Arc::default();
        let id = SessionRef::new(ProviderKind::Codex, "parent".into()).unwrap();
        let other = SessionRef::new(ProviderKind::Codex, "other".into()).unwrap();
        let page = Arc::make_mut(state.lists.threads.as_mut().unwrap());
        page.revision = 7;
        page.limit = 2;
        page.has_more = true;
        page.has_more_projects = true;
        page.data = vec![
            Thread {
                id: Some(id.clone()),
                name: Some("Kept title".into()),
                cwd: Some("/work".into()),
                git_branch: Some("main".into()),
                has_changes: Some(false),
                list_revision: 1,
                updated_at: Some(1.),
                ..Default::default()
            },
            Thread {
                id: Some(other.clone()),
                name: Some("Other".into()),
                list_revision: 1,
                updated_at: Some(2.),
                ..Default::default()
            },
        ];
        let mut incoming = page.data[0].clone();
        incoming.name = Some("Native title".into());
        incoming.preview = Some("updated preview".into());
        incoming.git_branch = None;
        incoming.has_changes = None;
        incoming.list_revision = 2;
        let (next, effects) = notification(
            &state,
            Notification::SessionUpdated {
                thread: Box::new(incoming.clone()),
                project: None,
            },
        );
        assert!(effects.is_empty());
        let page = next.lists.threads.as_ref().unwrap();
        assert_eq!(
            (
                page.revision,
                page.limit,
                page.has_more,
                page.has_more_projects
            ),
            (7, 2, true, true)
        );
        assert_eq!(
            page.data
                .iter()
                .map(|row| row.id.as_ref().unwrap())
                .collect::<Vec<_>>(),
            [&other, &id]
        );
        let row = &page.data[1];
        assert_eq!(
            (
                row.name.as_deref(),
                row.preview.as_deref(),
                row.git_branch.as_deref(),
                row.has_changes
            ),
            (
                Some("Kept title"),
                Some("updated preview"),
                Some("main"),
                Some(false)
            )
        );
        assert_eq!(
            state.lists.threads.as_ref().unwrap().data[0].list_revision,
            1
        );
        assert!(
            state.lists.threads.as_ref().unwrap().data[0]
                .preview
                .is_none()
        );
        assert!(Arc::ptr_eq(
            &state.lists.project_threads,
            &next.lists.project_threads
        ));
        for revision in [2, 0] {
            incoming.list_revision = revision;
            let (same, effects) = notification(
                &next,
                Notification::SessionUpdated {
                    thread: Box::new(incoming.clone()),
                    project: None,
                },
            );
            assert!(effects.is_empty());
            assert!(Arc::ptr_eq(
                same.lists.threads.as_ref().unwrap(),
                next.lists.threads.as_ref().unwrap()
            ));
            assert!(Arc::ptr_eq(
                &same.lists.project_threads,
                &next.lists.project_threads
            ));
        }
        let inserted = SessionRef::new(ProviderKind::Codex, "inserted".into()).unwrap();
        let (inserted_state, effects) = notification(
            &next,
            Notification::SessionUpdated {
                thread: Box::new(Thread {
                    id: Some(inserted.clone()),
                    name: Some("Inserted".into()),
                    updated_at: Some(4.),
                    list_revision: 1,
                    ..Default::default()
                }),
                project: None,
            },
        );
        assert!(effects.is_empty());
        let page = inserted_state.lists.threads.as_ref().unwrap();
        assert_eq!(
            page.data
                .iter()
                .map(|row| row.id.as_ref().unwrap())
                .collect::<Vec<_>>(),
            [&inserted, &other]
        );
        assert!(page.has_more);
        assert_eq!(
            next.lists.threads.as_ref().unwrap().data[1].id.as_ref(),
            Some(&id)
        );
    }

    #[test]
    fn rename_updates_the_first_qualifying_duplicate_and_only_mutates_matching_pages() {
        let id = SessionRef::new(ProviderKind::Codex, "duplicate".into()).unwrap();
        let other = SessionRef::new(ProviderKind::Codex, "other".into()).unwrap();
        let page = |data| {
            Arc::new(serde_json::from_value::<ThreadList>(json!({"revision":8,"limit":3,"data":data,"projects":[],"hasMore":true,"hasMoreProjects":true,"projectPages":{}})).unwrap())
        };
        let root = page(
            json!([{"id":id,"name":"Fresh","listRevision":3},{"id":id,"name":"Old","listRevision":1},{"id":other,"name":"Other","listRevision":1}]),
        );
        let untouched = page(json!([{"id":other,"name":"Other","listRevision":1}]));
        let state = Snapshot {
            lists: crate::state::Lists {
                threads: Some(root.clone()),
                project_threads: Arc::new(
                    [
                        ("matching".into(), root.clone()),
                        ("untouched".into(), untouched.clone()),
                    ]
                    .into(),
                ),
                ..Default::default()
            },
            ..Default::default()
        };
        let (next, effects) = notification(
            &state,
            Notification::SessionRenamed {
                session: id.clone(),
                name: Some("Renamed".into()),
                revision: 2,
            },
        );
        assert!(effects.is_empty());
        for page in [
            next.lists.threads.as_ref().unwrap(),
            &next.lists.project_threads["matching"],
        ] {
            assert_eq!(
                (page.data[0].name.as_deref(), page.data[0].list_revision),
                (Some("Fresh"), 3)
            );
            assert_eq!(
                (page.data[1].name.as_deref(), page.data[1].list_revision),
                (Some("Renamed"), 2)
            );
            assert_eq!(
                (
                    page.revision,
                    page.limit,
                    page.has_more,
                    page.has_more_projects
                ),
                (8, 3, true, true)
            );
        }
        assert_eq!(root.data[1].name.as_deref(), Some("Old"));
        assert!(Arc::ptr_eq(
            &next.lists.project_threads["untouched"],
            &untouched
        ));
        let (same, effects) = notification(
            &next,
            Notification::SessionRenamed {
                session: id,
                name: None,
                revision: 2,
            },
        );
        assert!(effects.is_empty());
        assert!(Arc::ptr_eq(
            same.lists.threads.as_ref().unwrap(),
            next.lists.threads.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(
            &same.lists.project_threads,
            &next.lists.project_threads
        ));
    }
}

#[cfg(test)]
mod workspace_mutation_tests {
    use super::*;
    use crate::{
        models::{Item, ItemBody, ItemStatus},
        session::SessionChange,
    };

    #[test]
    fn only_finished_workspace_tools_refresh_the_tree_during_a_turn() {
        for body in [
            ItemBody::FileChange {
                changes: vec![],
                output: String::new(),
            },
            ItemBody::CommandExecution {
                command: "touch new.txt".into(),
                cwd: None,
                output: String::new(),
                exit_code: None,
            },
        ] {
            for (status, expected) in [
                (ItemStatus::Running, false),
                (ItemStatus::Completed, true),
                (ItemStatus::Failed, true),
            ] {
                let change = SessionChange::Item {
                    turn_id: "turn".into(),
                    item: Arc::new(Item::new("item".into(), status, body.clone())),
                };
                assert_eq!(workspace_mutated(&change), expected);
            }
        }
        let prose = SessionChange::Item {
            turn_id: "turn".into(),
            item: Arc::new(Item::new(
                "prose".into(),
                ItemStatus::Completed,
                ItemBody::AssistantText {
                    text: "hello".into(),
                    phase: crate::models::AssistantPhase::Final,
                },
            )),
        };
        assert!(!workspace_mutated(&prose));
    }
}
