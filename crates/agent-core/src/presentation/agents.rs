//! The selected conversation's agent fleet, independent of conversation navigation.
use super::task_title;
use crate::localization::UiLanguage;
use crate::{
    models::{Item, ItemBody, ItemStatus, SessionStatus, TurnStatus},
    session::{ProviderKind, SessionRef},
    state::Snapshot,
};
use std::collections::{BTreeMap, HashMap, HashSet};

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Clone, PartialEq)]
pub struct AgentSummary {
    pub key: String,
    pub title: String,
    pub status: String,
    pub active: bool,
    pub unread: bool,
    pub model: Option<String>,
    pub detail: Option<String>,
    pub depth: u32,
}

type AgentEntries = BTreeMap<String, (SessionRef, Option<SessionRef>, AgentSummary)>;

/// A child conversation named by a provider's subagent activity. A provider
/// without a conversation identity cannot offer navigation to that child.
#[derive(Clone, PartialEq)]
pub struct SubagentPresentation {
    pub target: Option<SessionRef>,
    pub name: String,
    pub message: String,
    pub status: TurnStatus,
}

impl SubagentPresentation {
    pub fn status(&self, child: Option<&crate::models::Thread>) -> TurnStatus {
        child
            .and_then(|child| child.turns.as_ref()?.last())
            .map(|turn| turn.status)
            .filter(|status| *status != TurnStatus::Unknown)
            .unwrap_or(self.status)
    }

    pub fn name(&self, child: Option<&crate::models::Thread>) -> String {
        child
            .and_then(|child| child.name.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| self.name.clone())
    }
}

/// Elapsed wall time of the child's own work. A partial history cannot give its
/// original start, and unknown timings stay unknown.
pub fn subagent_elapsed(
    language: UiLanguage,
    child: Option<&crate::models::Thread>,
    now_seconds: f64,
) -> Option<String> {
    let child = child?;
    if child.history_has_more == Some(true) {
        return None;
    }
    let turns = child.turns.as_ref()?;
    let starts = |turn: &crate::models::Turn| {
        turn.started_at_ms
            .map(|ms| ms as f64)
            .or_else(|| turn.started_at.map(|seconds| seconds * 1000.0))
            .filter(|ms| ms.is_finite())
    };
    let start = turns
        .iter()
        .filter_map(|turn| starts(turn))
        .min_by(f64::total_cmp);
    let end = turns
        .iter()
        .filter_map(|turn| {
            if turn.status == TurnStatus::Running {
                return Some(now_seconds * 1000.0);
            }
            turn.completed_at_ms
                .map(|ms| ms as f64)
                .or_else(|| Some(starts(turn)? + turn.duration_ms? as f64))
        })
        .filter(|ms| ms.is_finite())
        .max_by(f64::total_cmp);
    let elapsed = match (start, end) {
        (Some(start), Some(end)) => (end - start).max(0.0) as u64,
        _ if turns.len() == 1 => turns[0].duration_ms?,
        _ => return None,
    };
    Some(super::elapsed_text(language, elapsed))
}

pub(super) fn subagent_rows(language: UiLanguage, item: &Item) -> Vec<SubagentPresentation> {
    let ItemBody::Subagent {
        receivers,
        states,
        prompt,
        ..
    } = item.body()
    else {
        return Vec::new();
    };
    let mut targets: Vec<_> = receivers
        .iter()
        .chain(states.iter().map(|state| &state.session))
        .collect();
    let mut seen = HashSet::new();
    targets.retain(|target| seen.insert((*target).clone()));
    let row = |target: Option<&SessionRef>| {
        let state = states.iter().find(|state| Some(&state.session) == target);
        SubagentPresentation {
            target: target.cloned(),
            name: prompt
                .as_deref()
                .and_then(|text| text.lines().find(|line| !line.trim().is_empty()))
                .unwrap_or_else(|| language.text("work.subagent"))
                .to_owned(),
            message: state
                .and_then(|state| state.message.clone())
                .unwrap_or_default(),
            status: state
                .map(|state| state.status)
                .unwrap_or(if target.is_some() {
                    TurnStatus::Unknown
                } else {
                    match item.status {
                        ItemStatus::Running => TurnStatus::Running,
                        ItemStatus::Completed => TurnStatus::Completed,
                        ItemStatus::Failed => TurnStatus::Failed,
                        _ => TurnStatus::Unknown,
                    }
                }),
        }
    };
    if targets.is_empty() {
        vec![row(None)]
    } else {
        targets
            .into_iter()
            .map(|target| row(Some(target)))
            .collect()
    }
}

impl Snapshot {
    pub fn agent_panel(&self, pane: &crate::state::PaneState) -> Vec<AgentSummary> {
        let Some(root) = pane.navigation.thread_id.as_ref() else {
            return Vec::new();
        };
        let mut entries = AgentEntries::new();
        for thread in self.sessions.conversations.values() {
            let (Some(id), Some(parent)) = (&thread.id, &thread.parent_id) else {
                continue;
            };
            let observed = self.lists.activity.active.get(id).copied();
            let row = session_agent(
                self.catalog.ui_language,
                id,
                task_title(
                    self.catalog.ui_language,
                    thread.name.as_deref(),
                    thread.preview.as_deref(),
                ),
                match thread.status {
                    SessionStatus::Running => TurnStatus::Running,
                    _ => TurnStatus::Unknown,
                },
                observed.or_else(|| (thread.status == SessionStatus::Idle).then_some(false)),
                self.lists.activity.unread.contains(id),
            );
            entries.insert(row.key.clone(), (parent.clone(), Some(id.clone()), row));
        }
        for (owner, item, provider) in self.conversation_items() {
            let ItemBody::Subagent {
                tool,
                prompt,
                model,
                receivers,
                states,
                result,
                ..
            } = item.body()
            else {
                continue;
            };
            let detail = result
                .as_ref()
                .and_then(serde_json::Value::as_str)
                .or(prompt.as_deref())
                .map(str::to_owned);
            if receivers.is_empty() {
                if provider != ProviderKind::Claude {
                    continue;
                }
                let row = local_agent(
                    self.catalog.ui_language,
                    owner,
                    &item.id,
                    item.status,
                    prompt.as_deref(),
                    model.clone(),
                    detail,
                );
                entries.insert(row.key.clone(), (owner.clone(), None, row));
            } else {
                for receiver in receivers {
                    if receiver.provider != Some(provider) {
                        continue;
                    }
                    let key = session_key(receiver);
                    if !entries.contains_key(&key) && tool != "spawnAgent" {
                        continue;
                    }
                    let entry = entries.entry(key).or_insert_with(|| {
                        let row = session_agent(
                            self.catalog.ui_language,
                            receiver,
                            &receiver.id,
                            TurnStatus::Unknown,
                            self.lists.activity.active.get(receiver).copied(),
                            self.lists.activity.unread.contains(receiver),
                        );
                        (owner.clone(), Some(receiver.clone()), row)
                    });
                    if &entry.0 != owner {
                        continue;
                    }
                    if model.is_some() {
                        entry.2.model = model.clone();
                    }
                    if let Some(state) = states.iter().find(|state| &state.session == receiver) {
                        let (status, active) = agent_status(
                            state.status,
                            self.lists.activity.active.get(receiver).copied(),
                        );
                        entry.2.status = self.catalog.ui_language.text(status).into();
                        entry.2.active = active;
                        entry.2.detail = state.message.clone().or_else(|| detail.clone());
                    } else if detail.is_some() {
                        entry.2.detail = detail.clone();
                    }
                }
            }
        }
        scoped_agents(root, entries)
    }
}

pub(super) fn empty() -> std::sync::Arc<Vec<AgentSummary>> {
    use std::sync::{Arc, LazyLock};
    static EMPTY: LazyLock<Arc<Vec<AgentSummary>>> = LazyLock::new(Arc::default);
    EMPTY.clone()
}

pub(super) fn project(
    snapshot: &Snapshot,
    pane: &crate::state::PaneState,
    previous: Option<&super::view::Presentation>,
) -> std::sync::Arc<Vec<AgentSummary>> {
    use crate::state::Same;
    use std::sync::Arc;
    if pane.observed_agents.is_none() || pane.observed_agents != pane.navigation.thread_id {
        return empty();
    }
    if let Some(previous) = previous
        && snapshot.catalog.ui_language == previous.snapshot.catalog.ui_language
        && pane.observed_agents == previous.pane().observed_agents
        && snapshot
            .sessions
            .conversations
            .same(&previous.snapshot.sessions.conversations)
        && snapshot
            .lists
            .activity
            .same(&previous.snapshot.lists.activity)
    {
        return previous.agents.clone();
    }
    let agents = snapshot.agent_panel(pane);
    if let Some(previous) = previous
        && agents == *previous.agents
    {
        return previous.agents.clone();
    }
    Arc::new(agents)
}

impl Snapshot {
    fn conversation_items(&self) -> impl Iterator<Item = (&SessionRef, &Item, ProviderKind)> {
        self.sessions
            .conversations
            .iter()
            .flat_map(|(owner, thread)| {
                thread.turns.iter().flatten().flat_map(move |turn| {
                    turn.items.iter().flatten().filter_map(move |item| {
                        Some((
                            owner,
                            item.as_ref(),
                            turn.provider
                                .or(thread.active_provider)
                                .or(owner.provider)?,
                        ))
                    })
                })
            })
    }
}

fn session_key(id: &SessionRef) -> String {
    format!("session:{id}")
}

fn session_agent(
    language: UiLanguage,
    id: &SessionRef,
    title: &str,
    status: TurnStatus,
    observed_active: Option<bool>,
    unread: bool,
) -> AgentSummary {
    let (status, active) = agent_status(status, observed_active);
    AgentSummary {
        key: session_key(id),
        title: title.into(),
        status: language.text(status).into(),
        active,
        unread,
        model: None,
        detail: None,
        depth: 0,
    }
}

fn local_agent(
    language: UiLanguage,
    owner: &SessionRef,
    item_id: &agent_protocol::ids::ItemId,
    item_status: ItemStatus,
    prompt: Option<&str>,
    model: Option<String>,
    detail: Option<String>,
) -> AgentSummary {
    let key = format!("item:{owner}:{item_id}");
    let (status, active) = agent_status(
        match item_status {
            ItemStatus::Running => TurnStatus::Running,
            ItemStatus::Completed => TurnStatus::Completed,
            ItemStatus::Failed => TurnStatus::Failed,
            ItemStatus::Interrupted | ItemStatus::Declined => TurnStatus::Interrupted,
            ItemStatus::Unknown => TurnStatus::Unknown,
        },
        None,
    );
    AgentSummary {
        key,
        title: prompt
            .map(crate::models::compact_title)
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| language.text("サブエージェント").into()),
        status: language.text(status).into(),
        active,
        unread: false,
        model,
        detail,
        depth: 0,
    }
}

fn agent_status(status: TurnStatus, observed_active: Option<bool>) -> (&'static str, bool) {
    let active = observed_active.unwrap_or(status == TurnStatus::Running);
    (
        if active {
            "実行中"
        } else {
            match status {
                TurnStatus::Completed => "完了",
                TurnStatus::Failed => "失敗",
                TurnStatus::Interrupted => "停止",
                TurnStatus::Running => "待機",
                TurnStatus::Unknown if observed_active == Some(false) => "待機",
                TurnStatus::Unknown => "未確認",
            }
        },
        active,
    )
}

fn scoped_agents(root: &SessionRef, entries: AgentEntries) -> Vec<AgentSummary> {
    let mut children: HashMap<_, Vec<_>> = HashMap::new();
    for (_, (parent, session, row)) in entries {
        children.entry(parent).or_default().push((session, row));
    }
    let mut pending: Vec<_> = children
        .remove(root)
        .unwrap_or_default()
        .into_iter()
        .rev()
        .map(|(session, row)| (session, row, 0))
        .collect();
    let mut seen = HashSet::from([root.clone()]);
    let mut rows = Vec::new();
    while let Some((session, mut row, depth)) = pending.pop() {
        if let Some(session) = session {
            if !seen.insert(session.clone()) {
                continue;
            }
            pending.extend(
                children
                    .remove(&session)
                    .unwrap_or_default()
                    .into_iter()
                    .rev()
                    .map(|(session, row)| (session, row, depth + 1)),
            );
        }
        row.depth = depth;
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Item, ItemContent, Thread, Turn};
    use proptest::prelude::*;
    use std::sync::Arc;

    #[test]
    fn elapsed_uses_child_wall_time_and_freezes_when_the_child_finishes() {
        let mut child = Thread {
            turns: Some(vec![
                Arc::new(Turn {
                    status: TurnStatus::Completed,
                    started_at_ms: Some(10_000),
                    completed_at_ms: Some(12_000),
                    ..Default::default()
                }),
                Arc::new(Turn {
                    status: TurnStatus::Running,
                    started_at: Some(20.0),
                    ..Default::default()
                }),
            ]),
            ..Default::default()
        };
        assert_eq!(
            subagent_elapsed(UiLanguage::English, Some(&child), 25.0).as_deref(),
            Some("15s")
        );
        assert_eq!(
            subagent_elapsed(UiLanguage::English, Some(&child), 26.0).as_deref(),
            Some("16s")
        );
        let turn = Arc::make_mut(&mut child.turns.as_mut().unwrap()[1]);
        turn.status = TurnStatus::Completed;
        turn.completed_at_ms = Some(27_000);
        assert_eq!(
            subagent_elapsed(UiLanguage::English, Some(&child), 99.0).as_deref(),
            Some("17s")
        );
        child.history_has_more = Some(true);
        assert!(subagent_elapsed(UiLanguage::English, Some(&child), 99.0).is_none());
        assert!(subagent_elapsed(UiLanguage::English, Some(&Thread::default()), 99.0).is_none());
        assert!(subagent_elapsed(UiLanguage::English, None, 99.0).is_none());
    }

    #[test]
    fn subagent_timing_keeps_unknown_data_unknown_and_clamps_clock_skew() {
        let child = |turn: Turn| Thread {
            turns: Some(vec![Arc::new(turn)]),
            ..Default::default()
        };
        assert_eq!(
            subagent_elapsed(
                UiLanguage::English,
                Some(&child(Turn {
                    started_at_ms: Some(10_000),
                    duration_ms: Some(2_500),
                    ..Default::default()
                })),
                100.0
            )
            .as_deref(),
            Some("2s")
        );
        assert_eq!(
            subagent_elapsed(
                UiLanguage::English,
                Some(&child(Turn {
                    duration_ms: Some(2_500),
                    ..Default::default()
                })),
                100.0
            )
            .as_deref(),
            Some("2s")
        );
        assert_eq!(
            subagent_elapsed(
                UiLanguage::English,
                Some(&child(Turn {
                    started_at_ms: Some(30_000),
                    completed_at_ms: Some(20_000),
                    ..Default::default()
                })),
                100.0
            )
            .as_deref(),
            Some("0s")
        );
        assert!(
            subagent_elapsed(UiLanguage::English, Some(&child(Turn::default())), 100.0).is_none()
        );
        let agent = SubagentPresentation {
            target: None,
            name: "Review".into(),
            message: String::new(),
            status: TurnStatus::Completed,
        };
        assert_eq!(
            agent.status(Some(&child(Turn {
                status: TurnStatus::Running,
                ..Default::default()
            }))),
            TurnStatus::Running
        );
        assert_eq!(
            agent.status(Some(&child(Turn::default()))),
            TurnStatus::Completed
        );
        assert_eq!(
            agent.name(Some(&Thread {
                name: Some("Reviewer".into()),
                ..Default::default()
            })),
            "Reviewer"
        );
        assert_eq!(
            agent.name(Some(&Thread {
                name: Some("  ".into()),
                ..Default::default()
            })),
            "Review"
        );
    }

    fn session(id: &str) -> SessionRef {
        SessionRef {
            provider: Some(ProviderKind::Codex),
            id: id.into(),
        }
    }

    fn snapshot(data: serde_json::Value) -> Snapshot {
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).thread_id =
            Some(session("root"));
        let data: Vec<Thread> = serde_json::from_value(data).unwrap();
        snapshot.sessions.conversations = Arc::new(
            data.iter()
                .filter_map(|thread| Some((thread.id.clone()?, Arc::new(thread.clone()))))
                .collect(),
        );
        snapshot.lists.threads = Some(Arc::new(
            serde_json::from_value(serde_json::json!({
            "data":data.into_iter().filter(|thread| thread.parent_id.is_none()).collect::<Vec<_>>(),
            "projects":[],  "hasMore":false,"hasMoreProjects":false,"limit":5, }))
            .unwrap(),
        ));
        snapshot
    }

    #[test]
    fn projected_fleet_tracks_observation_and_activity_without_rebuilding_for_input() {
        use crate::presentation::view::Presentation;
        use crate::state::Draft;
        let mut state = snapshot(serde_json::json!([
            {"id":{"provider":"codex","id":"root"}},
            {"id":{"provider":"codex","id":"child"},"parentId":{"provider":"codex","id":"root"},"name":"Review","status":"running"}
        ]));
        let hidden = Presentation::project(Arc::new(state.clone()), "", None).unwrap();
        assert!(hidden.agents.is_empty());
        Arc::make_mut(state.panes.primary_mut()).observed_agents = Some(session("root"));
        let opened = Presentation::project(Arc::new(state.clone()), "", Some(&hidden)).unwrap();
        assert!(opened.changes(hidden).agents);
        assert_eq!(opened.agents.len(), 1);
        assert!(opened.agents[0].active);
        let mut foreign = state.clone();
        Arc::make_mut(foreign.panes.primary_mut()).observed_agents = Some(session("other"));
        let foreign = Presentation::project(Arc::new(foreign), "", Some(&opened)).unwrap();
        assert!(
            foreign.agents.is_empty(),
            "another conversation's observation cannot expose this fleet"
        );
        Arc::make_mut(&mut state.input.drafts).insert(
            state.panes.primary().navigation.draft_key.clone(),
            Arc::new(Draft {
                text: "typed".into(),
                ..Default::default()
            }),
        );
        let typed = Presentation::project(Arc::new(state.clone()), "", Some(&opened)).unwrap();
        assert!(Arc::ptr_eq(&opened.agents, &typed.agents));
        Arc::make_mut(
            Arc::make_mut(&mut state.sessions.conversations)
                .get_mut(&session("child"))
                .unwrap(),
        )
        .preview = Some("A streaming preview".into());
        let streamed = Presentation::project(Arc::new(state.clone()), "", Some(&typed)).unwrap();
        assert!(Arc::ptr_eq(&typed.agents, &streamed.agents));
        assert!(!streamed.changes(typed.clone()).agents);
        Arc::make_mut(&mut state.lists.activity)
            .active
            .insert(session("child"), false);
        let stopped = Presentation::project(Arc::new(state.clone()), "", Some(&typed)).unwrap();
        assert!(stopped.changes(typed).agents);
        assert!(!stopped.agents[0].active);
        Arc::make_mut(state.panes.primary_mut()).observed_agents = None;
        let closed = Presentation::current(Arc::new(state.clone()), &stopped).unwrap();
        assert!(
            closed.agents.is_empty(),
            "closing must not wait for the worker"
        );
        let projected = Presentation::project(Arc::new(state), "", Some(&closed)).unwrap();
        assert!(!projected.changes(closed).agents);
    }

    #[test]
    fn language_changes_refresh_agent_copy_and_keep_names_and_identity() {
        use crate::presentation::view::Presentation;
        let mut state = snapshot(serde_json::json!([
            {"id":{"provider":"codex","id":"root"}},
            {"id":{"provider":"codex","id":"named"},"parentId":{"provider":"codex","id":"root"},"name":"設定\n{0}","status":"running"},
            {"id":{"provider":"codex","id":"unnamed"},"parentId":{"provider":"codex","id":"root"},"status":"idle"}
        ]));
        Arc::make_mut(state.panes.primary_mut()).observed_agents = Some(session("root"));
        let japanese = Presentation::project(Arc::new(state.clone()), "", None).unwrap();
        assert_eq!(japanese.agents[0].status, "実行中");
        assert_eq!(japanese.agents[1].title, "無題のタスク");
        state.catalog.ui_language = UiLanguage::English;
        let english = Presentation::project(Arc::new(state.clone()), "", Some(&japanese)).unwrap();
        assert!(english.changes(japanese.clone()).agents);
        assert_eq!(english.agents[0].title, "設定\n{0}");
        assert_eq!(english.agents[0].status, "Running");
        assert_eq!(english.agents[1].title, "Untitled task");
        assert_eq!(english.agents[1].status, "Idle");
        assert_eq!(english.agents[0].key, japanese.agents[0].key);
        assert_eq!(english.agents[1].key, japanese.agents[1].key);
        assert_eq!(japanese.agents[0].status, "実行中");
        let same = Presentation::project(Arc::new(state), "", Some(&english)).unwrap();
        assert!(Arc::ptr_eq(&same.agents, &english.agents));
    }

    #[test]
    fn agents_are_scoped_to_the_selected_parent_and_keep_stable_identity() {
        let mut state = snapshot(serde_json::json!([
            {"id":{"provider":"codex","id":"root"}},
            {"id":{"provider":"codex","id":"child"},"parentId":{"provider":"codex","id":"root"},"name":"Review","status":"running"},
            {"id":{"provider":"codex","id":"grandchild"},"parentId":{"provider":"codex","id":"child"},"name":"Check"},
            {"id":{"provider":"codex","id":"other"},"parentId":{"provider":"codex","id":"unrelated"}},
            {"id":{"provider":"codex","id":"fork"}},
            {"id":{"provider":"claude","id":"child"},"parentId":{"provider":"claude","id":"root"}},
            {"id":{"provider":"codex","id":"cross"},"parentId":{"provider":"claude","id":"root"}},
            {"id":{"provider":"codex","id":"self"},"parentId":{"provider":"codex","id":"self"}}
        ]));
        let before = state.agent_panel(state.panes.primary());
        assert_eq!(
            before
                .iter()
                .map(|row| (row.title.as_str(), row.depth))
                .collect::<Vec<_>>(),
            [("Review", 0), ("Check", 1)]
        );
        assert!(before[0].active);
        assert_eq!(before[1].status, "未確認");
        assert_eq!(
            state.panes.primary().navigation.thread_id,
            Some(session("root"))
        );
        for thread in Arc::make_mut(&mut state.sessions.conversations).values_mut() {
            Arc::make_mut(thread).updated_at = Some(999.);
        }
        Arc::make_mut(&mut state.lists.activity)
            .active
            .insert(session("child"), false);
        let after = state.agent_panel(state.panes.primary());
        assert_eq!(
            before.iter().map(|row| &row.key).collect::<Vec<_>>(),
            after.iter().map(|row| &row.key).collect::<Vec<_>>()
        );
        assert!(!after[0].active);
        assert_eq!(after[0].status, "待機");
        Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).navigation).thread_id =
            Some(session("fork"));
        assert!(state.agent_panel(state.panes.primary()).is_empty());
        Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).navigation).thread_id = None;
        assert!(state.agent_panel(state.panes.primary()).is_empty());
    }

    fn item(
        id: &str,
        status: ItemStatus,
        tool: &str,
        receivers: Vec<SessionRef>,
        states: Vec<crate::models::SubagentState>,
    ) -> Arc<Item> {
        Arc::new(Item {
            id: id.into(),
            status,
            client_input_id: None,
            body: ItemContent::Inline {
                body: Box::new(ItemBody::Subagent {
                    tool: tool.into(),
                    prompt: Some("Check the change".into()),
                    model: Some("review-model".into()),
                    effort: None,
                    sender: None,
                    receivers,
                    states,
                    agent_id: None,
                    result: None,
                }),
            },
        })
    }

    fn put_items(state: &mut Snapshot, owner: SessionRef, items: Vec<Arc<Item>>) {
        Arc::make_mut(&mut state.sessions.conversations).insert(
            owner,
            Arc::new(Thread {
                turns: Some(vec![Arc::new(Turn {
                    id: "turn".into(),
                    items: Some(items),
                    ..Default::default()
                })]),
                ..Default::default()
            }),
        );
    }

    #[test]
    fn native_events_enrich_one_agent_without_reparenting_it() {
        let mut state = snapshot(serde_json::json!([
            {"id":{"provider":"codex","id":"child"},"parentId":{"provider":"codex","id":"root"},"name":"Review","status":"running"}
        ]));
        put_items(
            &mut state,
            session("root"),
            vec![item(
                "spawn",
                ItemStatus::Completed,
                "spawnAgent",
                vec![session("child")],
                vec![crate::models::SubagentState {
                    session: session("child"),
                    status: TurnStatus::Completed,
                    message: Some("Finished review".into()),
                }],
            )],
        );
        put_items(
            &mut state,
            session("unrelated"),
            vec![item(
                "send",
                ItemStatus::Running,
                "sendInput",
                vec![session("child")],
                vec![],
            )],
        );
        Arc::make_mut(&mut state.lists.activity)
            .active
            .insert(session("child"), true);
        let agents = state.agent_panel(state.panes.primary());
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].title, "Review");
        assert_eq!(agents[0].model.as_deref(), Some("review-model"));
        assert_eq!(agents[0].detail.as_deref(), Some("Finished review"));
        assert_eq!(agents[0].status, "実行中");
        Arc::make_mut(&mut state.lists.activity)
            .active
            .insert(session("child"), false);
        assert_eq!(state.agent_panel(state.panes.primary())[0].status, "完了");
        Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).navigation).thread_id =
            Some(session("unrelated"));
        assert!(state.agent_panel(state.panes.primary()).is_empty());
    }

    #[test]
    fn spawn_is_visible_before_list_refresh_but_wait_does_not_invent_agents() {
        let mut state = snapshot(serde_json::json!([]));
        put_items(
            &mut state,
            session("root"),
            vec![
                item(
                    "spawn",
                    ItemStatus::Completed,
                    "spawnAgent",
                    vec![session("child")],
                    vec![],
                ),
                item(
                    "wait",
                    ItemStatus::Running,
                    "wait",
                    vec![session("other")],
                    vec![],
                ),
                item("empty", ItemStatus::Running, "wait", vec![], vec![]),
                item(
                    "foreign",
                    ItemStatus::Completed,
                    "spawnAgent",
                    vec![SessionRef {
                        provider: Some(ProviderKind::Claude),
                        id: "child".into(),
                    }],
                    vec![],
                ),
            ],
        );
        Arc::make_mut(&mut state.lists.activity)
            .active
            .insert(session("child"), true);
        let agents = state.agent_panel(state.panes.primary());
        assert_eq!(agents.len(), 1);
        assert!(agents[0].active);
    }

    #[test]
    fn claude_activity_updates_the_same_row_without_a_native_child_session() {
        let owner = SessionRef {
            provider: Some(ProviderKind::Claude),
            id: "root".into(),
        };
        let mut state = snapshot(serde_json::json!([]));
        Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).navigation).thread_id =
            Some(owner.clone());
        put_items(
            &mut state,
            owner.clone(),
            vec![item("task", ItemStatus::Running, "Task", vec![], vec![])],
        );
        let running = state.agent_panel(state.panes.primary());
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].title, "Check the change");
        assert!(running[0].active);
        put_items(
            &mut state,
            owner,
            vec![item("task", ItemStatus::Completed, "Task", vec![], vec![])],
        );
        let completed = state.agent_panel(state.panes.primary());
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].key, running[0].key);
        assert_eq!(completed[0].status, "完了");
        assert!(!completed[0].active);
        state.catalog.ui_language = UiLanguage::English;
        let english = state.agent_panel(state.panes.primary());
        assert_eq!(english[0].status, "Done");
        assert_eq!(english[0].title, "Check the change");
        assert_eq!(english[0].key, completed[0].key);
        assert!(state.thread_list().unwrap().threads.is_empty());
    }

    #[rstest::rstest]
    #[case(TurnStatus::Running, None, "実行中", true)]
    #[case(TurnStatus::Running, Some(false), "待機", false)]
    #[case(TurnStatus::Completed, None, "完了", false)]
    #[case(TurnStatus::Failed, None, "失敗", false)]
    #[case(TurnStatus::Interrupted, None, "停止", false)]
    #[case(TurnStatus::Unknown, None, "未確認", false)]
    #[case(TurnStatus::Unknown, Some(false), "待機", false)]
    #[case(TurnStatus::Completed, Some(true), "実行中", true)]
    fn status_uses_live_observations(
        #[case] status: TurnStatus,
        #[case] observed: Option<bool>,
        #[case] label: &str,
        #[case] active: bool,
    ) {
        assert_eq!(agent_status(status, observed), (label, active));
    }

    #[rstest::rstest]
    #[case(SessionStatus::Running, "実行中", true)]
    #[case(SessionStatus::Idle, "待機", false)]
    #[case(SessionStatus::Unavailable, "未確認", false)]
    #[case(SessionStatus::Unknown, "未確認", false)]
    fn native_status_is_visible_without_parent_events(
        #[case] status: SessionStatus,
        #[case] label: &str,
        #[case] active: bool,
    ) {
        let mut state = snapshot(serde_json::json!([]));
        Arc::make_mut(&mut state.sessions.conversations).insert(
            session("child"),
            Arc::new(Thread {
                id: Some(session("child")),
                parent_id: Some(session("root")),
                status,
                ..Default::default()
            }),
        );
        let agents = state.agent_panel(state.panes.primary());
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].status, label);
        assert_eq!(agents[0].active, active);
    }

    proptest! {
        #[test]
        fn arbitrary_native_graphs_show_only_unique_reachable_agents(parents in prop::collection::vec(0usize..25, 1..25)) {
            let mut state = snapshot(serde_json::json!([]));
            Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).navigation).thread_id = Some(session("0"));
            state.sessions.conversations = Arc::new(parents.iter().enumerate().map(|(index,parent)| {
                let id = session(&index.to_string());
                (id.clone(), Arc::new(Thread {
                    id:Some(id), parent_id:Some(session(&parent.to_string())), ..Default::default()
                }))
            }).collect());
            let rows = state.agent_panel(state.panes.primary());
            let expected:HashSet<_> = (1..parents.len()).filter(|&index| {
                let mut cursor=index;
                let mut visited=HashSet::new();
                while cursor != 0 {
                    if cursor >= parents.len() || !visited.insert(cursor) { return false; }
                    cursor=parents[cursor];
                }
                true
            }).map(|index| format!("session:{}", session(&index.to_string()))).collect();
            let actual:HashSet<_> = rows.iter().map(|row| row.key.clone()).collect();
            prop_assert_eq!(rows.len(),actual.len());
            prop_assert_eq!(actual,expected);
        }
    }
}
