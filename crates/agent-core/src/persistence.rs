//! Device storage is an explicit subset of runtime state. Host results and
//! connection authority are fetched again, never restored from this format.
use crate::state::{
    Activity, Draft, FileDraft, ModelDefaults, ModelDefaultsScope, Navigation, PendingSubmission,
    ScopedData, Snapshot,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};

/// Required user-owned fields prevent a damaged document from silently becoming
/// an empty draft. Runtime Snapshot fields cannot change the storage contract.
#[derive(Serialize, Deserialize)]
pub struct PersistedState {
    follow_up_behavior: crate::state::FollowUpBehavior,
    model_defaults: ModelDefaults,
    #[serde(with = "entries")]
    scoped_model_defaults: Arc<BTreeMap<ModelDefaultsScope, ModelDefaults>>,
    storage_scope: String,
    archived_scopes: Arc<BTreeMap<String, Arc<ScopedData>>>,
    #[serde(with = "entries")]
    drafts: Arc<BTreeMap<crate::state::DraftKey, Arc<Draft>>>,
    #[serde(with = "entries")]
    queue_edits: Arc<BTreeMap<crate::state::DraftKey, Arc<Draft>>>,
    pending_submissions: Arc<BTreeMap<agent_protocol::ids::ClientInputId, Arc<PendingSubmission>>>,
    file_drafts: Arc<BTreeMap<String, Arc<FileDraft>>>,
    navigation: Arc<Navigation>,
    activity: Arc<Activity>,
}

impl PersistedState {
    pub fn capture(snapshot: &Snapshot) -> Self {
        Self {
            follow_up_behavior: snapshot.follow_up_behavior,
            model_defaults: snapshot.model_defaults.clone(),
            scoped_model_defaults: snapshot.scoped_model_defaults.clone(),
            storage_scope: snapshot.storage_scope.clone(),
            archived_scopes: snapshot.archived_scopes.clone(),
            drafts: snapshot.drafts.clone(),
            queue_edits: snapshot.queue_edits.clone(),
            pending_submissions: snapshot.pending_submissions.clone(),
            file_drafts: snapshot.file_drafts.clone(),
            navigation: snapshot.navigation.clone(),
            activity: snapshot.activity.clone(),
        }
    }
}

pub fn encode(snapshot: &Snapshot) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&PersistedState::capture(snapshot))
}

/// Device-owned preferences shared by every saved Host snapshot.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClientPreferences {
    follow_up_behavior: crate::state::FollowUpBehavior,
    #[serde(flatten)]
    defaults: ModelDefaults,
    #[serde(with = "entries")]
    scoped: Arc<BTreeMap<ModelDefaultsScope, ModelDefaults>>,
}
impl ClientPreferences {
    pub fn capture(snapshot: &Snapshot) -> Self {
        Self {
            follow_up_behavior: snapshot.follow_up_behavior,
            defaults: snapshot.model_defaults.clone(),
            scoped: snapshot.scoped_model_defaults.clone(),
        }
    }

    pub fn apply(&self, snapshot: &mut Snapshot) {
        snapshot.follow_up_behavior = self.follow_up_behavior;
        snapshot.model_defaults = self.defaults.clone();
        snapshot.scoped_model_defaults = self.scoped.clone();
    }
}
pub fn encode_client_preferences(snapshot: &Snapshot) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&ClientPreferences::capture(snapshot))
}

pub fn apply_client_preferences(
    persisted: &[u8],
    defaults: &[u8],
) -> Result<Vec<u8>, serde_json::Error> {
    let mut saved = if persisted.is_empty() {
        PersistedState::capture(&Snapshot::default())
    } else {
        serde_json::from_slice::<PersistedState>(persisted)?
    };
    let preferences: ClientPreferences = if defaults.is_empty() {
        ClientPreferences::default()
    } else {
        serde_json::from_slice(defaults)?
    };
    saved.model_defaults = preferences.defaults;
    saved.scoped_model_defaults = preferences.scoped;
    saved.follow_up_behavior = preferences.follow_up_behavior;
    serde_json::to_vec(&saved)
}

pub fn decode(bytes: &[u8]) -> Result<Snapshot, serde_json::Error> {
    if bytes.is_empty() {
        return Ok(Snapshot::default());
    }
    let saved: PersistedState = serde_json::from_slice(bytes)?;
    Ok(Snapshot {
        follow_up_behavior: saved.follow_up_behavior,
        model_defaults: saved.model_defaults,
        scoped_model_defaults: saved.scoped_model_defaults,
        storage_scope: saved.storage_scope,
        archived_scopes: saved.archived_scopes,
        drafts: saved.drafts,
        queue_edits: saved.queue_edits,
        pending_submissions: saved.pending_submissions,
        file_drafts: saved.file_drafts,
        navigation: saved.navigation,
        activity: saved.activity,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Event, FollowUpBehavior, Intent, reduce};

    #[test]
    fn client_follow_up_setting_survives_restart_and_host_switch_without_changing_user_work() {
        let mut host = Snapshot {
            storage_scope: "other-host".into(),
            ..Default::default()
        };
        Arc::make_mut(&mut host.drafts).insert(
            "existing".into(),
            Arc::new(Draft {
                text: "keep this Host's draft".into(),
                ..Default::default()
            }),
        );
        let (device, effects) = reduce(
            &Snapshot::default(),
            Event::Intent(Intent::SetFollowUpBehavior {
                behavior: FollowUpBehavior::Steer,
            }),
        );
        assert!(effects.is_empty(), "a device setting performs no Host RPC");
        let preferences = encode_client_preferences(&device).unwrap();
        let restored =
            decode(&apply_client_preferences(&encode(&host).unwrap(), &preferences).unwrap())
                .unwrap();
        assert_eq!(restored.follow_up_behavior, FollowUpBehavior::Steer);
        assert_eq!(
            restored,
            Snapshot {
                follow_up_behavior: FollowUpBehavior::Steer,
                ..host
            }
        );
        assert_eq!(decode(&encode(&restored).unwrap()).unwrap(), restored);
        let fresh = decode(&apply_client_preferences(&[], &preferences).unwrap()).unwrap();
        assert_eq!(fresh.follow_up_behavior, FollowUpBehavior::Steer);
        assert!(fresh.drafts.is_empty());
        let mut damaged: serde_json::Value = serde_json::from_slice(&preferences).unwrap();
        damaged
            .as_object_mut()
            .unwrap()
            .remove("follow_up_behavior");
        assert!(apply_client_preferences(&[], &serde_json::to_vec(&damaged).unwrap()).is_err());
    }
}

/// Structured domain keys are stored as entries, without encoding keys into strings.
pub(crate) mod entries {
    use serde::{Deserialize, Serialize};
    use std::{collections::BTreeMap, sync::Arc};
    pub fn serialize<S: serde::Serializer, K: Serialize, V: Serialize>(
        values: &BTreeMap<K, V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(values.iter())
    }
    pub fn deserialize<
        'de,
        D: serde::Deserializer<'de>,
        K: Deserialize<'de> + Ord,
        V: Deserialize<'de>,
    >(
        deserializer: D,
    ) -> Result<Arc<BTreeMap<K, V>>, D::Error> {
        let entries = Vec::<(K, V)>::deserialize(deserializer)?;
        let mut values = BTreeMap::new();
        for (key, value) in entries {
            if values.insert(key, value).is_some() {
                return Err(serde::de::Error::custom("duplicate domain key"));
            }
        }
        Ok(Arc::new(values))
    }
}
