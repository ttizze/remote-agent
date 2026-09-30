use agent_core::{
    persistence,
    state::{Draft, Snapshot},
};
use serde_json::json;
use std::sync::Arc;

#[test]
fn runtime_fields_cannot_override_restored_user_work() {
    let mut snapshot = Snapshot::default();
    Arc::make_mut(&mut snapshot.drafts).insert(
        agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "thread".into(),
        }
        .into(),
        Arc::new(Draft {
            text: "keep this draft".into(),
            ..Default::default()
        }),
    );
    let mut saved: serde_json::Value =
        serde_json::from_slice(&persistence::encode(&snapshot).unwrap()).unwrap();
    saved["conversations"] = json!("invalid discardable history");
    saved["workspace"] = json!(42);
    saved["connected"] = json!(true);
    saved["epoch"] = json!(99);
    saved["requests"] = json!({"untrusted": "approval"});
    let restored = persistence::decode(&serde_json::to_vec(&saved).unwrap()).unwrap();
    assert_eq!(restored, snapshot);
}

#[test]
fn missing_or_invalid_user_work_is_not_silently_discarded() {
    let saved: serde_json::Value =
        serde_json::from_slice(&persistence::encode(&Snapshot::default()).unwrap()).unwrap();
    for field in [
        "storage_scope",
        "archived_scopes",
        "drafts",
        "pending_submissions",
        "file_drafts",
        "navigation",
        "activity",
    ] {
        let mut missing = saved.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            persistence::decode(&serde_json::to_vec(&missing).unwrap()).is_err(),
            "missing {field}"
        );
        let mut damaged = saved.clone();
        damaged[field] = json!(17);
        assert!(
            persistence::decode(&serde_json::to_vec(&damaged).unwrap()).is_err(),
            "damaged {field}"
        );
    }
    assert!(persistence::decode(b"{").is_err());
    assert_eq!(persistence::decode(b"").unwrap(), Snapshot::default());
}
