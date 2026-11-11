//! Compact task facts shared by every connection, Live Activities and APNs.
use super::*;

/// Receipts and task facts stay atomic with one another. Disk waits never hold
/// the connection/subscription mutex. Lock order is actor -> activity -> state.
#[derive(Default)]
pub(super) struct ActivityState {
    pub(super) apns: Option<Arc<crate::apns::Apns>>,
    pub(super) tasks: HashMap<SessionRef, Task>,
    pub(super) reads: read_state::ReadState,
    pub(super) task_revision: u64,
}

#[derive(Default)]
pub(super) struct Task {
    pub(super) facts: Option<TaskState>,
    pub(super) facts_revision: u64,
}
/// Only user conversations contribute to system surfaces. Subagent work,
/// requests and completions belong inside their conversation, not its task count.
fn task_display(
    tasks: &HashMap<SessionRef, Task>,
    unread: &[(SessionRef, agent_protocol::ids::TurnId)],
) -> agent_protocol::live_activity::TaskActivityDisplay {
    let mut summary = agent_protocol::live_activity::TaskActivitySummary::from_statuses(
        tasks
            .values()
            .filter_map(|task| task.facts.map(|facts| facts.phase().0)),
    );
    summary.finished = unread
        .iter()
        .filter(|(session, _)| {
            tasks.get(session).is_some_and(|task| {
                task.facts.is_none_or(|facts| {
                    facts.session_status() == agent_protocol::models::SessionStatus::Idle
                })
            })
        })
        .count() as u32;
    summary.display()
}
pub(super) fn task_activity_state(
    revision: u64,
    tasks: &HashMap<SessionRef, Task>,
    unread: Vec<(SessionRef, agent_protocol::ids::TurnId)>,
) -> agent_protocol::live_activity::TaskActivityState {
    let unread: Vec<_> = unread
        .into_iter()
        .filter(|(session, _)| tasks.contains_key(session))
        .collect();
    let statuses: std::collections::BTreeMap<_, _> = tasks
        .iter()
        .filter_map(|(id, task)| Some((id.clone(), task.facts?.session_status())))
        .collect();
    let mut waiting: Vec<_> = tasks
        .iter()
        .filter(|(_, task)| task.facts.is_some_and(|facts| facts.waiting))
        .map(|(session, _)| session.clone())
        .collect();
    waiting.sort();
    let mut background: Vec<_> = tasks
        .iter()
        .filter_map(|(session, task)| Some((session.clone(), task.facts?.background?)))
        .collect();
    background.sort();
    let mut attention: Vec<_> = tasks
        .iter()
        .filter_map(|(session, task)| Some((session.clone(), task.facts?.attention()?)))
        .collect();
    attention.sort_by(|left, right| left.0.cmp(&right.0));
    agent_protocol::live_activity::TaskActivityState {
        attention,
        revision,
        display: task_display(tasks, &unread),
        statuses: statuses.into_iter().collect(),
        waiting,
        background,
        unread,
    }
}
pub(super) fn publish_task_activity(
    apns: Option<&crate::apns::Apns>,
    sessions: &HashMap<SessionId, Outbound>,
    state: &agent_protocol::live_activity::TaskActivityState,
) -> Vec<SessionId> {
    if let Some(apns) = apns {
        apns.update(state.display.clone());
    }
    let frame = protocol::encode(Notification::TaskActivity {
        state: state.clone(),
    })
    .expect("task activity encodes");
    sessions
        .iter()
        .filter_map(|(id, output)| output.try_send(*id, frame.clone()).err().map(|_| *id))
        .collect()
}

fn background_wait(tasks: &[agent_protocol::execution::BackgroundTask]) -> Option<bool> {
    (!tasks.is_empty()).then(|| agent_protocol::execution::background_holds_completion(tasks))
}

/// Whether a request awaiting its answer passes `kind`.
fn awaits<K>(
    requests: &std::collections::BTreeMap<K, std::sync::Arc<agent_protocol::requests::Request>>,
    kind: impl Fn(&agent_protocol::requests::Request) -> bool,
) -> bool {
    requests.values().any(|request| {
        request.delivery == agent_protocol::session::RequestDelivery::Awaiting && kind(request)
    })
}

fn asks_permission(request: &agent_protocol::requests::Request) -> bool {
    use agent_protocol::requests::RequestBody;
    matches!(
        request.body,
        RequestBody::Approval { .. } | RequestBody::Permission { .. }
    )
}

/// Record a conversation's latest execution facts. Unknown facts never replace
/// a known phase that has not finished. Returns the resulting activity update.
pub(super) fn record_task_facts(
    state: &mut ActivityState,
    target: &SessionRef,
    timeline: &agent_protocol::session::Timeline,
    change: &SessionChange,
) -> Option<agent_protocol::live_activity::TaskActivityState> {
    // Provider events also contain subagents. Only conversations admitted by
    // the normal list (or created by the Host) have system task state.
    let Some(previous) = state.tasks.get(target) else {
        return None;
    };
    let previous = previous.facts;
    let task = TaskState {
        status: if timeline.status != agent_protocol::models::SessionStatus::Unknown
            || matches!(change, SessionChange::Status { .. })
        {
            timeline.status
        } else {
            previous.map(|task| task.status).unwrap_or_default()
        },
        waiting: awaits(&timeline.requests, |_| true),
        approval: awaits(&timeline.requests, asks_permission),
        background: background_wait(&timeline.background),
        latest: timeline
            .turns
            .as_ref()
            .and_then(|turns| turns.last())
            .map(|turn| turn.status)
            .or_else(|| previous.and_then(|task| task.latest)),
    };
    let completed = match change {
        SessionChange::Turn {
            completed: true,
            turn,
        } if turn.status == agent_protocol::execution::TurnStatus::Completed => Some(&turn.id),
        _ => None,
    };
    let read_changed = state.reads.record(
        target,
        task.session_status() == agent_protocol::models::SessionStatus::Running,
        completed,
    );
    let facts_changed = previous != Some(task)
        && (task.phase().0 != "unknown" || previous.is_some_and(|task| task.phase().1));
    if read_changed || facts_changed {
        state.task_revision += 1;
        let revision = state.task_revision;
        // A new completion is fresh evidence even when its phase is unchanged.
        // An older running list must not clear that completion's unread badge.
        let owned = state.tasks.get_mut(target).unwrap();
        owned.facts = Some(task);
        owned.facts_revision = revision;
        let activity = task_activity_state(state.task_revision, &state.tasks, state.reads.unread());
        return Some(activity);
    }
    None
}

impl SessionRouter {
    pub(in crate::host_rpc) fn set_apns(&self, apns: Option<Arc<crate::apns::Apns>>) {
        lock_state(&self.activity).apns = apns;
    }
    pub(in crate::host_rpc) fn apns(&self) -> Option<Arc<crate::apns::Apns>> {
        lock_state(&self.activity).apns.clone()
    }
    pub(in crate::host_rpc) fn task_activity_revision(&self) -> u64 {
        lock_state(&self.activity).task_revision
    }
    /// Admit normal conversations from the provider's root list or Host creation.
    pub(in crate::host_rpc) fn task_activity(
        &self,
        conversations: Vec<Thread>,
        read_revision: u64,
    ) -> agent_protocol::live_activity::TaskActivityState {
        let tasks: Vec<_> = conversations
            .into_iter()
            .filter(|thread| thread.parent_id.is_none())
            .filter_map(|thread| {
                let target = thread.id.clone()?;
                let actor = self.actor(&target);
                let owned = lock_state(&actor);
                let task = if owned.timeline.status
                    == agent_protocol::models::SessionStatus::Unknown
                    && thread.list_stale == Some(true)
                {
                    None
                } else {
                    let thread = owned.overlay(thread);
                    Some(TaskState {
                        status: thread.status,
                        waiting: awaits(&thread.requests, |_| true),
                        approval: awaits(&thread.requests, asks_permission),
                        background: background_wait(&thread.background),
                        latest: thread
                            .turns
                            .as_ref()
                            .and_then(|turns| turns.last())
                            .map(|turn| turn.status),
                    })
                };
                drop(owned);
                prune(&self.state, &target, &actor);
                Some((target, task))
            })
            .collect();
        let mut state = lock_state(&self.activity);
        let before = state.task_revision;
        // Provider events received while loading the list take precedence, including completion.
        for (session, task) in tasks {
            let previous = state.tasks.get(&session);
            let previous_facts = previous.and_then(|old| old.facts);
            let accepts_facts = !previous.is_some_and(|old| old.facts_revision > read_revision)
                && task.is_some_and(|task| {
                    (task.phase().0 != "unknown" || previous_facts.is_some_and(|old| old.phase().1))
                        && (previous_facts.is_some() || task.phase().1)
                });
            let facts = if accepts_facts { task } else { previous_facts };
            let unchanged = previous.is_some() && previous_facts == facts;
            let read_changed = accepts_facts
                && state.reads.record(
                    &session,
                    facts.unwrap().session_status()
                        == agent_protocol::models::SessionStatus::Running,
                    None,
                );
            if unchanged && !read_changed {
                continue;
            }
            state.task_revision += 1;
            let revision = state.task_revision;
            let owned = state.tasks.entry(session).or_default();
            if owned.facts != facts {
                owned.facts = facts;
                owned.facts_revision = revision;
            }
        }
        let result = task_activity_state(state.task_revision, &state.tasks, state.reads.unread());
        let failed = if state.task_revision != before {
            publish_task_activity(
                state.apns.as_deref(),
                &lock_state(&self.state).sessions,
                &result,
            )
        } else {
            Vec::new()
        };
        drop(state);
        self.close_failed(failed);
        result
    }
    pub(in crate::host_rpc) fn register_live_activity(
        &self,
        connection: SessionId,
        params: &agent_protocol::live_activity::RegisterLiveActivity,
    ) -> Result<(), String> {
        let state = lock_state(&self.activity);
        let connections = lock_state(&self.state);
        let principal = connections
            .sessions
            .get(&connection)
            .filter(|output| output.alive.load(Relaxed))
            .ok_or("connection is closed")?;
        state.apns.as_ref().ok_or("APNs is unavailable")?.register(
            &principal.principal,
            params,
            task_display(&state.tasks, &state.reads.unread()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversations_waiting_on_background_work_name_whether_it_holds_completion() {
        use agent_protocol::execution::{BackgroundTask, BackgroundTaskKind::*};
        let session = |id: &str| {
            SessionRef::new(agent_protocol::session::ProviderKind::Claude, id.into()).unwrap()
        };
        let task = |kinds: &[agent_protocol::execution::BackgroundTaskKind]| Task {
            facts: Some(TaskState {
                status: agent_protocol::models::SessionStatus::Running,
                background: background_wait(
                    &kinds
                        .iter()
                        .map(|kind| BackgroundTask {
                            id: format!("{kind:?}"),
                            kind: *kind,
                            description: None,
                        })
                        .collect::<Vec<_>>(),
                ),
                ..Default::default()
            }),
            facts_revision: 1,
        };
        let tasks = HashMap::from([
            (session("working"), task(&[])),
            (session("server"), task(&[Command])),
            (session("agents"), task(&[Command, Subagent])),
        ]);
        let state = task_activity_state(1, &tasks, Vec::new());
        assert_eq!(
            state.background,
            [(session("agents"), true), (session("server"), false)]
        );
        assert_eq!(state.statuses.len(), 3, "waiting work still runs");
    }
}
