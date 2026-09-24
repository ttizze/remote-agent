//! Device storage is an explicit subset of runtime state. Host results and
//! connection authority are fetched again, never restored from this format.
use crate::state::{
    Activity, Draft, FileDraft, Navigation, PendingSubmission, ScopedData, Snapshot,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};

/// Required user-owned fields prevent a damaged document from silently becoming
/// an empty draft. Runtime Snapshot fields cannot change the storage contract.
#[derive(Serialize, Deserialize)]
pub struct PersistedState {
    storage_scope: String,
    archived_scopes: Arc<BTreeMap<String, Arc<ScopedData>>>,
    drafts: Arc<BTreeMap<String, Arc<Draft>>>,
    pending_submissions: Arc<BTreeMap<String, Arc<PendingSubmission>>>,
    file_drafts: Arc<BTreeMap<String, Arc<FileDraft>>>,
    navigation: Arc<Navigation>,
    activity: Arc<Activity>,
}

impl PersistedState {
    pub fn capture(snapshot: &Snapshot) -> Self {
        Self {
            storage_scope: snapshot.storage_scope.clone(),
            archived_scopes: snapshot.archived_scopes.clone(),
            drafts: snapshot.drafts.clone(),
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

pub fn decode(bytes: &[u8]) -> Result<Snapshot, serde_json::Error> {
    if bytes.is_empty() {
        return Ok(Snapshot::default());
    }
    let saved: PersistedState = serde_json::from_slice(bytes)?;
    Ok(Snapshot {
        storage_scope: saved.storage_scope,
        archived_scopes: saved.archived_scopes,
        drafts: saved.drafts,
        pending_submissions: saved.pending_submissions,
        file_drafts: saved.file_drafts,
        navigation: saved.navigation,
        activity: saved.activity,
        ..Default::default()
    })
}
