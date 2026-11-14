//! T3 Code's Waiting roster for Codex: the commands and subagents a completed
//! turn left running. Codex reports their ends as item updates; Stop ends the
//! commands through their background terminals.
//! https://github.com/pingdotgg/t3code/blob/dac8b11ddc25bd5da62a367fdb6c3de777ae2986/packages/shared/src/orchestrationV2PendingBackgroundWork.ts
use crate::providers::AgentChange;
use agent_protocol::{
    execution::{BackgroundTask, BackgroundTaskKind, ItemStatus, TurnStatus},
    ids::ItemId,
    items::{Item, ItemBody},
    session::{SessionChange, SessionRef},
};
use serde_json::Value;
use std::{collections::HashMap, sync::Mutex};

#[derive(Default)]
pub(super) struct Rosters(Mutex<HashMap<SessionRef, Roster>>);

#[derive(Default)]
struct Roster {
    running: bool,
    work: Vec<Work>,
    published: Vec<BackgroundTask>,
    /// Prompts for commands that ended while a later turn ran.
    pending: Vec<String>,
}

struct Work {
    item: ItemId,
    task: BackgroundTask,
    /// The background terminal a command runs in.
    process: Option<String>,
    /// The work outlived the turn that started it.
    outlived: bool,
    /// Stop ended it; nothing reports on it.
    stopped: bool,
}

/// What a native change means for a conversation's background work.
#[derive(Default)]
pub(super) struct Observation {
    pub roster: Option<AgentChange>,
    /// As T3 Code, a command that outlived its turn wakes Codex to report on
    /// it once it ends, with this prompt.
    pub continuations: Vec<String>,
}

impl Roster {
    fn tasks(&self) -> Vec<BackgroundTask> {
        self.work.iter().map(|work| work.task.clone()).collect()
    }

    fn track(&mut self, item: &Item, native: &Value) {
        let known = self
            .work
            .iter()
            .position(|work| work.item == item.id)
            .map(|index| self.work.remove(index));
        if item.status != ItemStatus::Running {
            if let (
                Some(work),
                ItemBody::CommandExecution {
                    command,
                    exit_code,
                    output,
                    ..
                },
            ) = (&known, item.body())
                && work.outlived
                && !work.stopped
            {
                self.pending.push(super::native::background_command_detail(
                    command, *exit_code, output,
                ));
            }
            return;
        }
        let (kind, description) = match item.body() {
            ItemBody::CommandExecution { command, .. } => {
                (BackgroundTaskKind::Command, Some(command.as_str()))
            }
            ItemBody::Subagent { prompt, tool, .. } => (
                BackgroundTaskKind::Subagent,
                prompt.as_deref().or(Some(tool.as_str())),
            ),
            _ => return,
        };
        let (process, outlived, stopped) = known.map_or((None, false, false), |work| {
            (work.process, work.outlived, work.stopped)
        });
        self.work.push(Work {
            item: item.id.clone(),
            task: BackgroundTask {
                id: item.id.to_string(),
                kind,
                description: description
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned),
            },
            // A turn's summary of its items can omit what their start named.
            process: native["processId"].as_str().map(str::to_owned).or(process),
            outlived,
            stopped,
        });
    }
}

impl Rosters {
    /// The roster change and wake-ups a native change implies for its conversation.
    pub(super) fn observe(&self, change: &AgentChange, params: &Value) -> Observation {
        let AgentChange::Session { session, change } = change else {
            return Observation::default();
        };
        let mut rosters = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        let roster = rosters.entry(session.clone()).or_default();
        match change {
            SessionChange::Turn { turn, completed } => {
                roster.running = !completed;
                for item in turn.items.iter().flatten() {
                    roster.track(item, &Value::Null);
                }
                if *completed {
                    for work in &mut roster.work {
                        work.outlived = true;
                    }
                }
                // A stopped or failed turn leaves nothing it will report on.
                if *completed && turn.status != TurnStatus::Completed {
                    roster.work.clear();
                }
            }
            SessionChange::Item { item, .. } => roster.track(item, &params["item"]),
            SessionChange::Status { status }
                if !agent_protocol::models::task_active(None, *status) =>
            {
                roster.running = false;
            }
            _ => return Observation::default(),
        }
        // Wake-ups wait behind a running turn, as T3 Code queues them after it.
        let continuations = if roster.running {
            Vec::new()
        } else {
            std::mem::take(&mut roster.pending)
        };
        let waiting = roster.tasks();
        let roster = (waiting != roster.published).then(|| {
            roster.published = waiting.clone();
            AgentChange::Session {
                session: session.clone(),
                change: SessionChange::Background { tasks: waiting },
            }
        });
        Observation {
            roster,
            continuations,
        }
    }

    /// The background terminals Stop ends while the conversation waits. What
    /// it ends wakes Codex for nothing.
    pub(super) fn stop(&self, session: &SessionRef) -> Vec<String> {
        let mut rosters = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        let Some(roster) = rosters.get_mut(session).filter(|roster| !roster.running) else {
            return Vec::new();
        };
        roster
            .work
            .iter_mut()
            .filter_map(|work| {
                let process = work.process.clone()?;
                work.stopped = true;
                Some(process)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{models::Turn, session::ProviderKind};
    use serde_json::json;
    use std::sync::Arc;

    fn session() -> SessionRef {
        SessionRef::new(ProviderKind::Codex, "thread".into()).unwrap()
    }

    fn item(id: &str, status: ItemStatus, body: ItemBody) -> AgentChange {
        AgentChange::Session {
            session: session(),
            change: SessionChange::Item {
                turn_id: "turn".into(),
                item: Arc::new(Item::new(id.into(), status, body)),
            },
        }
    }

    fn command() -> ItemBody {
        ItemBody::CommandExecution {
            output_omitted: false,
            command: "npm run dev".into(),
            cwd: None,
            output: String::new(),
            exit_code: None,
        }
    }

    fn turn(status: TurnStatus) -> AgentChange {
        AgentChange::Session {
            session: session(),
            change: SessionChange::Turn {
                turn: Turn {
                    id: "turn".into(),
                    status,
                    ..Default::default()
                },
                completed: status != TurnStatus::Running,
            },
        }
    }

    fn roster(observation: Observation) -> Option<Vec<(String, BackgroundTaskKind)>> {
        match observation.roster? {
            AgentChange::Session {
                change: SessionChange::Background { tasks },
                ..
            } => Some(
                tasks
                    .into_iter()
                    .map(|task| (task.description.unwrap_or_default(), task.kind))
                    .collect(),
            ),
            _ => None,
        }
    }

    #[test]
    fn running_work_remains_owned_across_foreground_changes_until_it_ends() {
        let rosters = Rosters::default();
        assert_eq!(
            roster(rosters.observe(&turn(TurnStatus::Running), &Value::Null)),
            None
        );
        let started = json!({"item":{"processId":"process-1"}});
        assert_eq!(
            roster(rosters.observe(&item("server", ItemStatus::Running, command()), &started)),
            Some(vec![("npm run dev".into(), BackgroundTaskKind::Command)]),
            "the Host needs the roster while foreground work continues"
        );
        rosters.observe(
            &item(
                "agent",
                ItemStatus::Running,
                ItemBody::Subagent {
                    tool: "spawnAgent".into(),
                    prompt: Some("Review the change".into()),
                    model: None,
                    effort: None,
                    sender: None,
                    receivers: vec![],
                    states: vec![],
                    agent_id: None,
                    result: None,
                },
            ),
            &Value::Null,
        );
        rosters.observe(&item("done", ItemStatus::Running, command()), &Value::Null);
        assert_eq!(
            roster(rosters.observe(
                &item("done", ItemStatus::Completed, command()),
                &Value::Null
            )),
            Some(vec![
                ("npm run dev".into(), BackgroundTaskKind::Command),
                ("Review the change".into(), BackgroundTaskKind::Subagent),
            ])
        );
        assert_eq!(
            roster(rosters.observe(&turn(TurnStatus::Completed), &Value::Null)),
            None,
            "completing the foreground does not recreate its existing tasks"
        );
        assert_eq!(rosters.stop(&session()), ["process-1"]);
        assert_eq!(
            roster(rosters.observe(&item("server", ItemStatus::Failed, command()), &Value::Null)),
            Some(vec![(
                "Review the change".into(),
                BackgroundTaskKind::Subagent
            )])
        );
        assert!(rosters.stop(&session()).is_empty());
        assert_eq!(
            roster(rosters.observe(&turn(TurnStatus::Interrupted), &Value::Null)),
            Some(vec![]),
            "a stopped turn reports on nothing"
        );
    }

    #[test]
    fn a_command_that_outlived_its_turn_wakes_codex_once_no_turn_runs() {
        let finished = |exit_code| {
            item(
                "server",
                ItemStatus::Completed,
                ItemBody::CommandExecution {
                    output_omitted: false,
                    command: "npm run dev".into(),
                    cwd: None,
                    output: "ready\n".into(),
                    exit_code,
                },
            )
        };
        let rosters = Rosters::default();
        rosters.observe(&turn(TurnStatus::Running), &Value::Null);
        rosters.observe(
            &item("inside", ItemStatus::Running, command()),
            &Value::Null,
        );
        assert!(
            rosters
                .observe(
                    &item("inside", ItemStatus::Completed, command()),
                    &Value::Null
                )
                .continuations
                .is_empty(),
            "work that ends within its turn is part of the answer"
        );
        rosters.observe(
            &item("server", ItemStatus::Running, command()),
            &Value::Null,
        );
        rosters.observe(&turn(TurnStatus::Completed), &Value::Null);
        assert_eq!(
            rosters
                .observe(&finished(Some(0)), &Value::Null)
                .continuations,
            ["Background command completed (exit 0): npm run dev\n\nOutput tail:\nready"]
        );

        rosters.observe(
            &item("server", ItemStatus::Running, command()),
            &Value::Null,
        );
        rosters.observe(&turn(TurnStatus::Completed), &Value::Null);
        rosters.observe(&turn(TurnStatus::Running), &Value::Null);
        assert!(
            rosters
                .observe(&finished(Some(1)), &Value::Null)
                .continuations
                .is_empty(),
            "a running turn goes first"
        );
        assert_eq!(
            rosters
                .observe(&turn(TurnStatus::Completed), &Value::Null)
                .continuations
                .len(),
            1
        );
    }
}
