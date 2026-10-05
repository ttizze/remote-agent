//! One device preference owner shared by independent conversation views.
use super::*;
use crate::persistence::ClientPreferences;
use std::sync::{MutexGuard, Weak};

pub struct DevicePreferences {
    state: Mutex<State>,
}
pub(super) struct State {
    preferences: ClientPreferences,
    stores: Vec<Weak<Store>>,
}
impl DevicePreferences {
    pub fn new(preferences: ClientPreferences) -> Self {
        Self {
            state: Mutex::new(State {
                preferences,
                stores: Vec::new(),
            }),
        }
    }

    pub fn capture(&self) -> ClientPreferences {
        self.lock().preferences.clone()
    }

    /// Register before publishing a view. Its private draft/navigation remain
    /// untouched; stale restored device settings cannot replace this owner.
    pub fn attach(self: &Arc<Self>, store: &Arc<Store>) {
        let mut state = self.lock();
        let mut owner = store.preferences.lock().unwrap();
        assert!(owner.is_none(), "a Store already has a device owner");
        *owner = Some(self.clone());
        drop(owner);
        state.stores.retain(|store| store.strong_count() > 0);
        state.stores.push(Arc::downgrade(store));
        publish(store, &state.preferences);
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }
}
impl State {
    pub(super) fn publish(&mut self, preferences: ClientPreferences) {
        if self.preferences == preferences {
            return;
        }
        self.preferences = preferences;
        self.stores.retain(|view| {
            let Some(view) = view.upgrade() else {
                return false;
            };
            publish(&view, &self.preferences);
            !view.stop.is_cancelled()
        });
    }
}
fn publish(store: &Store, preferences: &ClientPreferences) {
    let publications = store.publications.lock().unwrap().clone();
    if let Some(publications) = publications {
        publications.send_if_modified(|current| {
            if store.stop.is_cancelled() || ClientPreferences::capture(current) == *preferences {
                return false;
            }
            let mut next = (**current).clone();
            preferences.apply(&mut next);
            publish_locked(current, next)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Draft, FollowUpBehavior, ModelDefaultsScope};

    #[tokio::test]
    async fn windows_share_preferences_without_sharing_drafts_or_restoring_stale_values() {
        let device = Arc::new(DevicePreferences::new(ClientPreferences::default()));
        let main = Arc::new(Store::offline(Snapshot::default()));
        let side = Arc::new(Store::offline(Snapshot::default()));
        device.attach(&main);
        device.attach(&side);
        let mut updates = side.subscribe();
        main.dispatch(Intent::SetFollowUpBehavior {
            behavior: FollowUpBehavior::Steer,
        })
        .await
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), updates.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updates.borrow().follow_up_behavior, FollowUpBehavior::Steer);
        side.dispatch(Intent::SelectDefaultEffort {
            scope: ModelDefaultsScope::Global,
            effort: Some("high".into()),
        })
        .await
        .unwrap();
        side.dispatch(Intent::SetDraft {
            thread_id: "side".into(),
            draft: Draft {
                text: "private draft".into(),
                ..Default::default()
            },
        })
        .await
        .unwrap();
        assert_eq!(
            main.snapshot().model_defaults.effort.as_deref(),
            Some("high")
        );
        assert_eq!(side.snapshot().follow_up_behavior, FollowUpBehavior::Steer);
        assert!(main.snapshot().drafts.is_empty());
        let restored = Arc::new(Store::offline(Snapshot::default()));
        device.attach(&restored);
        assert_eq!(
            restored.snapshot().follow_up_behavior,
            FollowUpBehavior::Steer
        );
        assert_eq!(
            restored.snapshot().model_defaults.effort.as_deref(),
            Some("high")
        );
        main.close().await.unwrap();
        side.dispatch(Intent::SetFollowUpBehavior {
            behavior: FollowUpBehavior::Queue,
        })
        .await
        .unwrap();
        assert_eq!(
            restored.snapshot().follow_up_behavior,
            FollowUpBehavior::Queue
        );
        assert!(
            main.dispatch(Intent::SetFollowUpBehavior {
                behavior: FollowUpBehavior::Steer,
            })
            .await
            .is_err()
        );
        assert_eq!(side.snapshot().follow_up_behavior, FollowUpBehavior::Queue);
        assert_eq!(
            side.snapshot().drafts[&crate::state::DraftKey::from("side")].text,
            "private draft"
        );
        let bytes = serde_json::to_vec(&device.capture()).unwrap();
        let fresh = crate::persistence::decode(
            &crate::persistence::apply_client_preferences(&[], &bytes).unwrap(),
        )
        .unwrap();
        assert_eq!(fresh.follow_up_behavior, FollowUpBehavior::Queue);
        assert_eq!(fresh.model_defaults.effort.as_deref(), Some("high"));
        assert!(fresh.drafts.is_empty());
    }
}
