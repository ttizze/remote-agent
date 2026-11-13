//! One Host snapshot owns every pane; presentations retain immutable pane references.
use super::*;

#[derive(Debug, Clone, PartialEq)]
pub struct PaneStates(pub(crate) Arc<BTreeMap<String, Arc<PaneState>>>);
impl Default for PaneStates {
    fn default() -> Self {
        Self::with_primary(Arc::default())
    }
}
impl<'de> Deserialize<'de> for PaneStates {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Arc::<PaneState>::deserialize(deserializer).map(Self::with_primary)
    }
}
impl Serialize for PaneStates {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.primary().serialize(serializer)
    }
}
impl PaneStates {
    pub fn with_primary(primary: Arc<PaneState>) -> Self {
        Self(Arc::new(BTreeMap::from([(String::new(), primary)])))
    }
    pub fn get(&self, id: &str) -> Option<&Arc<PaneState>> {
        self.0.get(id)
    }
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Arc<PaneState>> {
        if !self.0.contains_key(id) {
            return None;
        }
        Arc::make_mut(&mut self.0).get_mut(id)
    }
    pub fn primary(&self) -> &Arc<PaneState> {
        self.get("").expect("primary pane always exists")
    }
    pub fn primary_mut(&mut self) -> &mut Arc<PaneState> {
        self.get_mut("").expect("primary pane always exists")
    }
}
impl Same for PaneStates {
    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || self.0.len() == other.0.len()
                && self
                    .0
                    .iter()
                    .zip(other.0.iter())
                    .all(|((id, pane), (other_id, other))| {
                        id == other_id && Arc::ptr_eq(pane, other)
                    })
    }
}
impl Snapshot {
    pub(crate) fn invalidate_pane(&mut self, id: &str) {
        if let Some(pane) = self.panes.get_mut(id) {
            Arc::make_mut(pane).epoch += 1;
            Arc::make_mut(&mut self.operations).retain(|key, _| key.pane() != Some(id));
        }
    }
    pub(crate) fn open_pane(&mut self, id: String, cwd: String) {
        let used: BTreeSet<_> = self.panes.0.values().map(|pane| pane.draft_slot).collect();
        let slot = (1..=used.len() as u64 + 1)
            .find(|slot| !used.contains(slot))
            .unwrap();
        let draft_key = self.input.new_chat(&cwd, slot);
        Arc::make_mut(&mut self.panes.0).insert(
            id,
            Arc::new(PaneState {
                draft_slot: slot,
                navigation: Arc::new(Navigation {
                    cwd,
                    draft_key,
                    ..Default::default()
                }),
                ..Default::default()
            }),
        );
    }
    pub(crate) fn close_pane(&mut self, id: &str) {
        if id.is_empty() || !self.panes.0.contains_key(id) {
            return;
        }
        Arc::make_mut(&mut self.panes.0).remove(id);
        Arc::make_mut(&mut self.operations).retain(|key, _| key.pane() != Some(id));
        let retained: BTreeSet<_> = self
            .panes
            .0
            .values()
            .filter_map(|pane| pane.navigation.thread_id.clone())
            .collect();
        let conversations = &self.sessions.conversations;
        let active = &self.lists.activity.active;
        Arc::make_mut(&mut self.sessions.subscriptions).retain(|id, _| {
            retained.contains(id)
                || active.get(id) == Some(&true)
                || conversations
                    .get(id)
                    .is_some_and(|thread| !thread.requests.is_empty())
        });
        super::outputs::retire(self);
    }
    pub(crate) fn selected_in_any_pane(&self, id: &crate::session::SessionRef) -> bool {
        self.panes
            .0
            .values()
            .any(|pane| pane.navigation.thread_id.as_ref() == Some(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspection_round_trip_restores_only_the_primary_pane() {
        let mut state = Snapshot::default();
        state.open_pane("side".into(), "/side".into());
        let restored: Snapshot =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
        assert!(restored.panes.get("side").is_none());
        assert_eq!(restored.panes.primary(), state.panes.primary());
        let mut reopened = restored;
        reopened.open_pane("replacement".into(), "/side".into());
        assert_ne!(
            reopened.panes.primary().draft_slot,
            reopened.panes.get("replacement").unwrap().draft_slot,
        );
    }

    #[rstest::rstest]
    #[case::visible_in_another_pane(true, false, false, true)]
    #[case::running_in_background(false, true, false, true)]
    #[case::awaiting_answer(false, false, true, true)]
    #[case::idle_and_unobserved(false, false, false, false)]
    fn close_keeps_needed_shared_subscriptions_and_releases_idle_ones(
        #[case] visible: bool,
        #[case] active: bool,
        #[case] awaiting: bool,
        #[case] retained: bool,
    ) {
        use agent_protocol::{
            requests::{Request, RequestBody, RequestTarget},
            session::RequestDelivery,
        };
        let id =
            crate::session::SessionRef::new(crate::session::ProviderKind::Codex, "shared".into())
                .unwrap();
        let mut state = Snapshot::default();
        state.open_pane("closing".into(), String::new());
        state.open_pane("survivor".into(), String::new());
        Arc::make_mut(&mut Arc::make_mut(state.panes.get_mut("closing").unwrap()).navigation)
            .thread_id = Some(id.clone());
        if visible {
            Arc::make_mut(
                &mut Arc::make_mut(state.panes.get_mut("survivor").unwrap()).navigation,
            )
            .thread_id = Some(id.clone());
        }
        if active {
            Arc::make_mut(&mut state.lists.activity)
                .active
                .insert(id.clone(), true);
        }
        if awaiting {
            Arc::make_mut(&mut state.sessions.conversations).insert(
                id.clone(),
                Arc::new(Thread {
                    id: Some(id.clone()),
                    requests: [(
                        "request".into(),
                        Arc::new(Request {
                            id: "request".into(),
                            target: RequestTarget::Session,
                            delivery: RequestDelivery::Awaiting,
                            body: RequestBody::ToolExecution {
                                tool: "read".into(),
                                namespace: None,
                                arguments: serde_json::json!({}),
                            },
                        }),
                    )]
                    .into(),
                    ..Default::default()
                }),
            );
        }
        Arc::make_mut(&mut state.sessions.subscriptions).insert(id.clone(), uuid::Uuid::new_v4());
        let primary = state.panes.primary().clone();
        let survivor = state.panes.get("survivor").unwrap().clone();
        state.close_pane("closing");
        assert_eq!(state.sessions.subscriptions.contains_key(&id), retained);
        assert!(state.panes.get("closing").is_none());
        assert!(Arc::ptr_eq(&primary, state.panes.primary()));
        assert!(Arc::ptr_eq(&survivor, state.panes.get("survivor").unwrap()));
    }
}
