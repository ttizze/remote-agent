//! The selected conversation's agent fleet, independent of conversation navigation.
use crate::{
    models::{ItemBody, ItemStatus, SessionStatus, TurnStatus, task_title},
    session::{ProviderKind, SessionRef},
    state::Snapshot,
};
use std::collections::{BTreeMap, HashMap, HashSet};

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
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

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn agent_panel(&self) -> Vec<AgentSummary> {
        let Some(root) = self.navigation.thread_id.as_ref() else {
            return Vec::new();
        };
        let mut entries = AgentEntries::new();
        for thread in self.conversations.values() {
            let (Some(id), Some(parent)) = (&thread.id, &thread.parent_id) else {
                continue;
            };
            if id.provider != parent.provider {
                continue;
            }
            let observed = self.activity.active.get(id).copied();
            let (status, active) = agent_status(
                match thread.status {
                    SessionStatus::Running => TurnStatus::Running,
                    _ => TurnStatus::Unknown,
                },
                observed.or_else(|| (thread.status == SessionStatus::Idle).then_some(false)),
            );
            let key = format!("session:{id}");
            entries.insert(
                key.clone(),
                (
                    parent.clone(),
                    Some(id.clone()),
                    AgentSummary {
                        key,
                        title: task_title(thread.name.as_deref(), thread.preview.as_deref()).into(),
                        status: status.into(),
                        active,
                        unread: self.activity.unread.contains(id),
                        model: None,
                        detail: None,
                        depth: 0,
                    },
                ),
            );
        }
        for (owner, thread) in self.conversations.iter() {
            for item in thread
                .turns
                .iter()
                .flatten()
                .flat_map(|turn| turn.items.iter().flatten())
            {
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
                    if owner.provider != ProviderKind::Claude {
                        continue;
                    }
                    let key = format!("item:{owner}:{}", item.id);
                    let (status, active) = agent_status(
                        match item.status {
                            ItemStatus::Running => TurnStatus::Running,
                            ItemStatus::Completed => TurnStatus::Completed,
                            ItemStatus::Failed => TurnStatus::Failed,
                            ItemStatus::Interrupted | ItemStatus::Declined => {
                                TurnStatus::Interrupted
                            }
                            ItemStatus::Unknown => TurnStatus::Unknown,
                        },
                        None,
                    );
                    entries.insert(
                        key.clone(),
                        (
                            owner.clone(),
                            None,
                            AgentSummary {
                                key,
                                title: prompt
                                    .as_deref()
                                    .map(crate::models::compact_title)
                                    .filter(|title| !title.is_empty())
                                    .unwrap_or_else(|| "サブエージェント".into()),
                                status: status.into(),
                                active,
                                unread: false,
                                model: model.clone(),
                                detail,
                                depth: 0,
                            },
                        ),
                    );
                } else {
                    for receiver in receivers {
                        if receiver.provider != owner.provider {
                            continue;
                        }
                        let key = format!("session:{receiver}");
                        if !entries.contains_key(&key) && tool != "spawnAgent" {
                            continue;
                        }
                        let entry = entries.entry(key.clone()).or_insert_with(|| {
                            let (status, active) = agent_status(
                                TurnStatus::Unknown,
                                self.activity.active.get(receiver).copied(),
                            );
                            (
                                owner.clone(),
                                Some(receiver.clone()),
                                AgentSummary {
                                    key,
                                    title: receiver.id.clone(),
                                    status: status.into(),
                                    active,
                                    unread: self.activity.unread.contains(receiver),
                                    model: None,
                                    detail: None,
                                    depth: 0,
                                },
                            )
                        });
                        if &entry.0 != owner {
                            continue;
                        }
                        if model.is_some() {
                            entry.2.model = model.clone();
                        }
                        if let Some(state) = states.iter().find(|state| &state.session == receiver)
                        {
                            let (status, active) = agent_status(
                                state.status,
                                self.activity.active.get(receiver).copied(),
                            );
                            entry.2.status = status.into();
                            entry.2.active = active;
                            entry.2.detail = state.message.clone().or_else(|| detail.clone());
                        } else if detail.is_some() {
                            entry.2.detail = detail.clone();
                        }
                    }
                }
            }
        }
        scoped_agents(root, entries)
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

    fn session(id: &str) -> SessionRef {
        SessionRef {
            provider: ProviderKind::Codex,
            id: id.into(),
        }
    }

    fn snapshot(data: serde_json::Value) -> Snapshot {
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.navigation).thread_id = Some(session("root"));
        let data: Vec<Thread> = serde_json::from_value(data).unwrap();
        snapshot.conversations = Arc::new(
            data.iter()
                .filter_map(|thread| Some((thread.id.clone()?, Arc::new(thread.clone()))))
                .collect(),
        );
        snapshot.threads = Some(Arc::new(
            serde_json::from_value(serde_json::json!({
            "data":data.into_iter().filter(|thread| thread.parent_id.is_none()).collect::<Vec<_>>(),
            "projects":[],  "hasMore":false,"hasMoreProjects":false, }))
            .unwrap(),
        ));
        snapshot
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
        let before = state.agent_panel();
        assert_eq!(
            before
                .iter()
                .map(|row| (row.title.as_str(), row.depth))
                .collect::<Vec<_>>(),
            [("Review", 0), ("Check", 1)]
        );
        assert!(before[0].active);
        assert_eq!(before[1].status, "未確認");
        assert_eq!(state.navigation.thread_id, Some(session("root")));
        for thread in Arc::make_mut(&mut state.conversations).values_mut() {
            Arc::make_mut(thread).updated_at = Some(999.);
        }
        Arc::make_mut(&mut state.activity)
            .active
            .insert(session("child"), false);
        let after = state.agent_panel();
        assert_eq!(
            before.iter().map(|row| &row.key).collect::<Vec<_>>(),
            after.iter().map(|row| &row.key).collect::<Vec<_>>()
        );
        assert!(!after[0].active);
        assert_eq!(after[0].status, "待機");
        Arc::make_mut(&mut state.navigation).thread_id = Some(session("fork"));
        assert!(state.agent_panel().is_empty());
        Arc::make_mut(&mut state.navigation).thread_id = None;
        assert!(state.agent_panel().is_empty());
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
        Arc::make_mut(&mut state.conversations).insert(
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
        Arc::make_mut(&mut state.activity)
            .active
            .insert(session("child"), true);
        let agents = state.agent_panel();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].title, "Review");
        assert_eq!(agents[0].model.as_deref(), Some("review-model"));
        assert_eq!(agents[0].detail.as_deref(), Some("Finished review"));
        assert_eq!(agents[0].status, "実行中");
        Arc::make_mut(&mut state.activity)
            .active
            .insert(session("child"), false);
        assert_eq!(state.agent_panel()[0].status, "完了");
        Arc::make_mut(&mut state.navigation).thread_id = Some(session("unrelated"));
        assert!(state.agent_panel().is_empty());
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
                        provider: ProviderKind::Claude,
                        id: "child".into(),
                    }],
                    vec![],
                ),
            ],
        );
        Arc::make_mut(&mut state.activity)
            .active
            .insert(session("child"), true);
        let agents = state.agent_panel();
        assert_eq!(agents.len(), 1);
        assert!(agents[0].active);
    }

    #[test]
    fn claude_activity_updates_the_same_row_without_a_native_child_session() {
        let owner = SessionRef {
            provider: ProviderKind::Claude,
            id: "root".into(),
        };
        let mut state = snapshot(serde_json::json!([]));
        Arc::make_mut(&mut state.navigation).thread_id = Some(owner.clone());
        put_items(
            &mut state,
            owner.clone(),
            vec![item("task", ItemStatus::Running, "Task", vec![], vec![])],
        );
        let running = state.agent_panel();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].title, "Check the change");
        assert!(running[0].active);
        put_items(
            &mut state,
            owner,
            vec![item("task", ItemStatus::Completed, "Task", vec![], vec![])],
        );
        let completed = state.agent_panel();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].key, running[0].key);
        assert_eq!(completed[0].status, "完了");
        assert!(!completed[0].active);
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
        Arc::make_mut(&mut state.conversations).insert(
            session("child"),
            Arc::new(Thread {
                id: Some(session("child")),
                parent_id: Some(session("root")),
                status,
                ..Default::default()
            }),
        );
        let agents = state.agent_panel();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].status, label);
        assert_eq!(agents[0].active, active);
    }

    proptest! {
        #[test]
        fn arbitrary_native_graphs_show_only_unique_reachable_agents(parents in prop::collection::vec(0usize..25, 1..25)) {
            let mut state = snapshot(serde_json::json!([]));
            Arc::make_mut(&mut state.navigation).thread_id = Some(session("0"));
            state.conversations = Arc::new(parents.iter().enumerate().map(|(index,parent)| {
                let id = session(&index.to_string());
                (id.clone(), Arc::new(Thread {
                    id:Some(id), parent_id:Some(session(&parent.to_string())), ..Default::default()
                }))
            }).collect());
            let rows = state.agent_panel();
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
