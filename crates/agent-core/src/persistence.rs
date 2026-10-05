//! Only device-owned drafts, navigation and unacknowledged commands are durable.
use crate::state::{Draft, Snapshot};
use orchestration::{Command, ThreadId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Default, Serialize, Deserialize)]
struct LocalState {
    drafts: BTreeMap<String, Draft>,
    default_draft: Draft,
    selected_thread: Option<ThreadId>,
    selected_project: Option<String>,
    pending_commands: Vec<Command>,
    pending_launches: Vec<agent_protocol::orchestration::LaunchThread>,
}
pub fn encode(snapshot: &Snapshot) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&LocalState {
        drafts: (*snapshot.drafts).clone(),
        default_draft: snapshot.default_draft.clone(),
        selected_thread: snapshot.selected_thread.clone(),
        selected_project: snapshot.selected_project.clone(),
        pending_commands: (*snapshot.pending_commands).clone(),
        pending_launches: (*snapshot.pending_launches).clone(),
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
        selected_thread: local.selected_thread,
        selected_project: local.selected_project,
        pending_commands: local.pending_commands.into(),
        pending_launches: local.pending_launches.into(),
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

/// Decode independent device-owned components; a damaged file must not lock out a Host.
pub fn recover(persisted: &[u8], defaults: &[u8]) -> Snapshot {
    let mut state = decode(persisted).unwrap_or_else(|_| Snapshot {
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
    state
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn device_drafts_and_pending_commands_roundtrip_without_host_cache() {
        let mut snapshot = Snapshot {
            connected: true,
            host_name: Some("Host".into()),
            ..Snapshot::default()
        };
        snapshot.drafts.insert(
            "new:bex:chats".into(),
            Draft {
                text: "Unsent message".into(),
                ..Draft::default()
            },
        );
        let restored = decode(&encode(&snapshot).unwrap()).unwrap();
        assert!(!restored.connected);
        assert!(restored.host_name.is_none());
        assert_eq!(restored.drafts["new:bex:chats"].text, "Unsent message");
        assert!(restored.shell.is_none());
    }
}
