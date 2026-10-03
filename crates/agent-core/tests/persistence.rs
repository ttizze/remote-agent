use agent_core::{
    persistence,
    state::{Draft, Snapshot},
};
use serde_json::json;
use std::sync::Arc;

#[test]
fn device_model_defaults_apply_across_hosts_without_replacing_their_user_work() {
    let defaults = agent_core::state::ModelDefaults {
        model: Some(agent_protocol::models::ModelRef {
            provider: agent_protocol::session::ProviderKind::Claude,
            id: "sonnet".into(),
        }),
        effort: Some("high".into()),
        service_tier: Some("fast".into()),
    };
    let preferences = serde_json::to_vec(&defaults).unwrap();
    for host in ["first", "second"] {
        let mut snapshot = Snapshot {
            storage_scope: host.into(),
            ..Default::default()
        };
        Arc::make_mut(&mut snapshot.drafts).insert(
            "existing".into(),
            Arc::new(Draft {
                text: format!("{host}'s draft"),
                effort: Some("medium".into()),
                ..Default::default()
            }),
        );
        let saved = persistence::encode(&snapshot).unwrap();
        let restored =
            persistence::decode(&persistence::apply_model_defaults(&saved, &preferences).unwrap())
                .unwrap();
        assert_eq!(
            restored,
            Snapshot {
                model_defaults: defaults.clone(),
                ..snapshot.clone()
            }
        );
        assert_eq!(
            persistence::decode(&persistence::apply_model_defaults(&saved, &[]).unwrap()).unwrap(),
            snapshot
        );
    }
    assert_eq!(
        persistence::decode(&persistence::apply_model_defaults(&[], &preferences).unwrap())
            .unwrap()
            .model_defaults,
        defaults
    );
    assert!(persistence::apply_model_defaults(b"{}", &preferences).is_err());
    assert!(persistence::apply_model_defaults(&[], b"invalid").is_err());
}

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

#[test]
fn unset_model_preferences_restore_user_work_with_automatic_defaults() {
    let mut snapshot = Snapshot::default();
    Arc::make_mut(&mut snapshot.drafts).insert(
        "draft".into(),
        Arc::new(Draft {
            text: "keep unsent input".into(),
            ..Default::default()
        }),
    );
    let mut saved: serde_json::Value =
        serde_json::from_slice(&persistence::encode(&snapshot).unwrap()).unwrap();
    saved.as_object_mut().unwrap().remove("model_defaults");
    let restored = persistence::decode(&serde_json::to_vec(&saved).unwrap()).unwrap();
    assert_eq!(restored, snapshot);
}
