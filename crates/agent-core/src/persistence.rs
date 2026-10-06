//! Device-owned state the native app stores: drafts, navigation, settings and
//! commands the Host has not confirmed. Host data lives in the disk cache.
use crate::commands::{build::FollowUpBehavior, outbox::Outbox};
use crate::state::{Draft, PendingRollback, Snapshot};
use agent_domain::{CommandId, ThreadId};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Default, Serialize, Deserialize)]
struct LocalState {
    drafts: BTreeMap<String, Draft>,
    default_draft: Draft,
    follow_up: FollowUpBehavior,
    selected_thread: Option<ThreadId>,
    selected_project: Option<String>,
    outbox: Outbox,
    rollbacks: BTreeMap<CommandId, PendingRollback>,
}

pub fn encode(snapshot: &Snapshot) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&LocalState {
        drafts: (*snapshot.drafts).clone(),
        default_draft: snapshot.default_draft.clone(),
        follow_up: snapshot.follow_up,
        selected_thread: snapshot.selected_thread.clone(),
        selected_project: snapshot.selected_project.clone(),
        outbox: snapshot.outbox.persisted(),
        rollbacks: snapshot.rollbacks.clone(),
    })
}

pub fn decode(bytes: &[u8]) -> Result<Snapshot, serde_json::Error> {
    let local: LocalState = if bytes.is_empty() {
        LocalState::default()
    } else {
        serde_json::from_slice(bytes)?
    };
    Ok(Snapshot {
        drafts: local.drafts.into(),
        default_draft: local.default_draft,
        follow_up: local.follow_up,
        selected_thread: local.selected_thread,
        selected_project: local.selected_project,
        outbox: Arc::new(local.outbox),
        rollbacks: local.rollbacks,
        ..Snapshot::default()
    })
}

pub fn encode_model_preferences(snapshot: &Snapshot) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&snapshot.default_draft)
}

pub fn apply_model_preferences(
    persisted: &[u8],
    defaults: &[u8],
) -> Result<Vec<u8>, serde_json::Error> {
    let mut state = recover(persisted, defaults);
    // Recovery warnings are runtime state, not saved data.
    state.error = None;
    encode(&state)
}

/// Decodes independent device-owned components; a damaged file must not lock
/// out a Host.
pub fn recover(persisted: &[u8], defaults: &[u8]) -> Snapshot {
    let mut state = decode(persisted).unwrap_or_else(|_| Snapshot {
        follow_up: FollowUpBehavior::default(),
        error: Some("Saved device state could not be read. Device drafts were reset.".into()),
        ..Default::default()
    });
    if !defaults.is_empty() {
        match serde_json::from_slice(defaults) {
            Ok(draft) => state.default_draft = draft,
            Err(_) => {
                state.error =
                    Some("Saved model preferences could not be read. Choose a model again.".into())
            }
        }
    }
    for draft in state.drafts.values_mut() {
        for attachment in &mut draft.attachments {
            if attachment.status == "uploading" {
                attachment.status = "failed".into();
                attachment.error = Some("Upload interrupted. Retry to continue.".into());
            }
        }
    }
    state.default_draft.attachments.clear();
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{build::dispatch, outbox::*};

    #[test]
    fn damaged_components_do_not_discard_valid_device_work_or_block_startup() {
        let mut state = Snapshot::default();
        state.default_draft.model = "valid model".into();
        state.drafts.insert(
            "thread".into(),
            Draft {
                text: "keep me".into(),
                ..Default::default()
            },
        );
        let recovered = recover(&encode(&state).unwrap(), b"broken preferences");
        assert_eq!(recovered.drafts["thread"].text, "keep me");
        assert_eq!(recovered.default_draft.model, "valid model");
        assert!(recovered.error.is_some());
        let recovered = recover(b"broken state", &encode_model_preferences(&state).unwrap());
        assert_eq!(recovered.default_draft.model, "valid model");
        assert!(recovered.error.is_some());
        assert!(decode(&apply_model_preferences(b"bad", b"bad").unwrap()).is_ok());
    }

    #[test]
    fn drafts_settings_and_unconfirmed_commands_roundtrip_without_host_data() {
        let mut snapshot = Snapshot {
            connected: true,
            host_name: Some("Host".into()),
            follow_up: FollowUpBehavior::Steer,
            ..Snapshot::default()
        };
        snapshot.drafts.insert(
            "new:chats".into(),
            Draft {
                text: "Unsent message".into(),
                ..Draft::default()
            },
        );
        let thread = ThreadId::new("thread").unwrap();
        let id = CommandId::new("command").unwrap();
        let mut outbox = Outbox::default();
        outbox
            .enqueue(PendingCommand::new(
                thread.clone(),
                Request::Dispatch(Box::new(dispatch(
                    thread,
                    id.clone(),
                    agent_domain::Command::MarkUnread,
                ))),
                agent_domain::Timestamp::from_millis(0).unwrap(),
            ))
            .unwrap();
        outbox.sending(&id);
        snapshot.outbox = Arc::new(outbox);
        let restored = decode(&encode(&snapshot).unwrap()).unwrap();
        assert!(!restored.connected);
        assert!(restored.host_name.is_none());
        assert_eq!(restored.drafts["new:chats"].text, "Unsent message");
        assert_eq!(restored.follow_up, FollowUpBehavior::Steer);
        assert!(restored.shell.snapshot.is_none());
        assert_eq!(restored.outbox.entries[0].phase, Phase::Queued);
    }

    #[test]
    fn a_new_device_defaults_to_queueing_follow_ups() {
        assert_eq!(decode(&[]).unwrap().follow_up, FollowUpBehavior::Queue);
    }
}
