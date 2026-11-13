//! Expanded command rows own ephemeral subscriptions independently of history.
use super::*;
use crate::peer::PeerError;
use agent_protocol::{
    conversation::records,
    sync::{Output, OutputUpdate},
};

fn desired(snapshot: &Snapshot, key: &records::OutputKey) -> bool {
    snapshot.panes.0.values().any(|pane| {
        pane.navigation.thread_id.as_ref() == Some(&key.session)
            && pane.output_items.contains(&key.item)
    }) && snapshot
        .sessions
        .conversations
        .get(&key.session)
        .into_iter()
        .flat_map(|thread| thread.turns.iter().flatten())
        .flat_map(|turn| turn.items.iter().flatten())
        .any(|item| {
            item.id == key.item
                && matches!(
                    item.status,
                    crate::models::ItemStatus::Running | crate::models::ItemStatus::Unknown
                )
                && matches!(
                    item.body(),
                    crate::models::ItemBody::CommandExecution { .. }
                )
        })
}

pub(super) fn watch(
    snapshot: &mut Snapshot,
    pane: &str,
    key: records::OutputKey,
    watching: bool,
) -> Vec<Effect> {
    if let Some(pane) = snapshot.panes.get_mut(pane).filter(|pane| {
        pane.navigation.thread_id.as_ref() == Some(&key.session)
            && pane.output_items.contains(&key.item) != watching
    }) {
        let items = Arc::make_mut(&mut Arc::make_mut(pane).output_items);
        if watching {
            items.insert(key.item.clone());
        } else {
            items.remove(&key.item);
        }
    }
    if !desired(snapshot, &key) {
        if snapshot.sessions.live_outputs.contains_key(&key) {
            Arc::make_mut(&mut snapshot.sessions.live_outputs).remove(&key);
        }
        return Vec::new();
    }
    if snapshot.connection.connected && !snapshot.sessions.live_outputs.contains_key(&key) {
        vec![Effect::execute(Subscribe { key })]
    } else {
        Vec::new()
    }
}

pub(super) fn reconnect(snapshot: &Snapshot) -> Vec<Effect> {
    let keys: BTreeSet<_> = snapshot
        .panes
        .0
        .values()
        .flat_map(|pane| {
            pane.navigation.thread_id.iter().flat_map(|session| {
                pane.output_items
                    .iter()
                    .map(move |item| records::OutputKey {
                        session: session.clone(),
                        item: item.clone(),
                    })
            })
        })
        .collect();
    keys.into_iter()
        .filter(|key| desired(snapshot, key))
        .map(|key| Effect::execute(Subscribe { key }))
        .collect()
}

pub(super) fn retire(snapshot: &mut Snapshot) {
    let keys: Vec<_> = snapshot
        .sessions
        .live_outputs
        .keys()
        .filter(|key| !desired(snapshot, key))
        .cloned()
        .collect();
    for key in keys {
        Arc::make_mut(&mut snapshot.sessions.live_outputs).remove(&key);
    }
}

pub(super) fn received(
    previous: &Snapshot,
    subscription: uuid::Uuid,
    update: OutputUpdate,
) -> (Snapshot, Vec<Effect>) {
    let Some((key, (_, tail))) = previous
        .sessions
        .live_outputs
        .iter()
        .find(|(_, (id, _))| *id == subscription)
    else {
        return (previous.clone(), Vec::new());
    };
    let mut next = previous.clone();
    match tail.apply(update) {
        Ok(output) => {
            Arc::make_mut(&mut next.sessions.live_outputs)
                .insert(key.clone(), (subscription, output));
            refresh(&mut next, &key.session);
            (next, Vec::new())
        }
        Err(_) => {
            Arc::make_mut(&mut next.sessions.live_outputs).remove(key);
            let effects = desired(&next, key)
                .then(|| Effect::execute(Subscribe { key: key.clone() }))
                .into_iter()
                .collect();
            (next, effects)
        }
    }
}

pub(super) fn overlay(
    mut thread: Thread,
    outputs: &BTreeMap<records::OutputKey, (uuid::Uuid, Output)>,
) -> Thread {
    let Some(session) = thread.id.as_ref() else {
        return thread;
    };
    for turn in thread.turns.iter_mut().flatten() {
        if !turn.items.iter().flatten().any(|item| {
            outputs.contains_key(&records::OutputKey {
                session: session.clone(),
                item: item.id.clone(),
            })
        }) {
            continue;
        }
        for item in Arc::make_mut(turn).items.iter_mut().flatten() {
            let Some((_, tail)) = outputs.get(&records::OutputKey {
                session: session.clone(),
                item: item.id.clone(),
            }) else {
                continue;
            };
            if matches!(
                item.status,
                crate::models::ItemStatus::Running | crate::models::ItemStatus::Unknown
            ) && let crate::models::ItemBody::CommandExecution {
                output,
                output_omitted,
                ..
            } = Arc::make_mut(item).body_mut()
            {
                *output = tail.text.clone();
                *output_omitted = tail.offset != 0;
            }
        }
    }
    thread
}

fn refresh(snapshot: &mut Snapshot, session: &crate::session::SessionRef) {
    if let Some(thread) = Arc::make_mut(&mut snapshot.sessions.conversations).get_mut(session) {
        *thread = Arc::new(overlay(
            thread.as_ref().clone(),
            &snapshot.sessions.live_outputs,
        ));
    }
}

#[derive(Debug, Clone)]
struct Subscribe {
    key: records::OutputKey,
}
impl op::Operation for Subscribe {
    const BACKGROUND: bool = true;
    const REPORT_ERROR: bool = false;
    fn navigation_bound(&self) -> bool {
        false
    }
    fn key(&self) -> Option<op::OperationKey> {
        Some(op::OperationKey::CommandOutput {
            session: self.key.session.clone(),
            item: self.key.item.clone(),
        })
    }
    type Input = ();
    fn capture(&self, snapshot: &Snapshot, _: &str) -> Result<(), PeerError> {
        if desired(snapshot, &self.key) {
            Ok(())
        } else {
            Err(PeerError::InvalidMessage(
                "command output is no longer visible".into(),
            ))
        }
    }
    type Output = records::OpenedOutput;
    async fn run(
        &self,
        _: (),
        context: &mut crate::store::Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        context
            .call(&records::SubscribeOutput {
                key: self.key.clone(),
                subscription: uuid::Uuid::nil(),
            })
            .await
    }
    fn apply(self, snapshot: &mut Snapshot, _: &str, output: Self::Output) -> Vec<Effect> {
        if desired(snapshot, &self.key) {
            Arc::make_mut(&mut snapshot.sessions.live_outputs)
                .insert(self.key.clone(), (output.subscription_id, output.initial));
            refresh(snapshot, &self.key.session);
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use op::Operation;

    #[test]
    fn live_output_is_local_to_its_visible_item_and_never_changes_the_row_cursor_or_cache() {
        use crate::models::{Item, ItemBody, ItemStatus, Turn, TurnStatus};
        let session =
            crate::session::SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
        let key = records::OutputKey {
            session: session.clone(),
            item: "command".into(),
        };
        let mut state = Snapshot::default();
        state.connection.connected = true;
        crate::test_conversations::open(
            &mut state,
            crate::models::ThreadResponse {
                thread: Thread {
                    id: Some(session.clone()),
                    active_provider: Some(crate::session::ProviderKind::Codex),
                    turns: Some(vec![Arc::new(Turn {
                        id: "turn".into(),
                        status: TurnStatus::Running,
                        items: Some(vec![Arc::new(Item::new(
                            key.item.clone(),
                            ItemStatus::Running,
                            ItemBody::CommandExecution {
                                command: "echo output".into(),
                                cwd: None,
                                output: String::new(),
                                output_omitted: true,
                                exit_code: None,
                            },
                        ))]),
                        ..Default::default()
                    })]),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let replica = state.sessions.replicas[&session].clone();
        assert_eq!(watch(&mut state, "", key.clone(), true).len(), 1);
        let subscription = uuid::Uuid::new_v4();
        Subscribe { key: key.clone() }.apply(
            &mut state,
            "",
            records::OpenedOutput {
                key: key.clone(),
                initial: Output::default(),
                subscription_id: subscription,
            },
        );
        let (mut live, effects) = received(
            &state,
            subscription,
            OutputUpdate::Append {
                offset: 0,
                text: "live command bytes".into(),
            },
        );
        assert!(effects.is_empty());
        assert!(Arc::ptr_eq(&live.sessions.replicas[&session], &replica));
        assert_eq!(live.sessions.replicas[&session].cursor(), replica.cursor());
        assert!(
            matches!(live.sessions.conversations[&session].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0].body(), ItemBody::CommandExecution { output, .. } if output == "live command bytes")
        );
        assert!(
            !String::from_utf8(crate::persistence::encode(&live).unwrap())
                .unwrap()
                .contains("live command bytes")
        );
        let (unchanged, _) = received(
            &live,
            uuid::Uuid::new_v4(),
            OutputUpdate::Tail(Output {
                offset: 0,
                text: "foreign".into(),
            }),
        );
        assert_eq!(unchanged, live);
        live.open_pane("side".into(), "/project".into());
        Arc::make_mut(live.panes.get_mut("side").unwrap()).navigation = Arc::new(Navigation {
            thread_id: Some(session.clone()),
            draft_key: session.clone().into(),
            ..Default::default()
        });
        assert!(watch(&mut live, "side", key.clone(), true).is_empty());
        assert!(watch(&mut live, "", key.clone(), false).is_empty());
        assert!(live.sessions.live_outputs.contains_key(&key));
        let (gap, effects) = received(
            &live,
            subscription,
            OutputUpdate::Append {
                offset: 1000,
                text: "missing".into(),
            },
        );
        assert_eq!(
            effects.len(),
            1,
            "a gap requests its own tail, independently of history"
        );
        assert!(gap.sessions.live_outputs.is_empty());
        assert!(Arc::ptr_eq(&gap.sessions.replicas[&session], &replica));
        live.close_pane("side");
        assert!(
            live.sessions.live_outputs.is_empty(),
            "the last view closes the resource subscription"
        );
    }
}
