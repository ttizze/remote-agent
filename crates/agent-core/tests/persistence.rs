use agent_core::{
    persistence,
    state::{Draft, Snapshot},
};
use serde_json::json;
use std::sync::Arc;

#[test]
fn runtime_fields_cannot_override_restored_user_work() {
    let mut snapshot = Snapshot::default();
    Arc::make_mut(&mut snapshot.input.drafts).insert(
        agent_protocol::session::SessionRef {
            provider: Some(agent_protocol::session::ProviderKind::Codex),
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
    saved["activity"] = json!(17);
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
fn pre_cutover_project_drafts_receive_a_persisted_address_without_losing_text() {
    use agent_core::state::{DraftKey, ScopedData};
    let mut snapshot = Snapshot::default();
    let old = DraftKey::from("new:/project");
    Arc::make_mut(&mut snapshot.input.drafts).insert(
        old.clone(),
        Arc::new(Draft {
            text: "unfinished".into(),
            ..Default::default()
        }),
    );
    Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).draft_key =
        old.clone();
    Arc::make_mut(&mut snapshot.lists.archived_scopes).insert(
        "other-host".into(),
        Arc::new(ScopedData {
            drafts: snapshot.input.drafts.clone(),
            navigation: snapshot.panes.primary().navigation.clone(),
            ..Default::default()
        }),
    );
    let restored = persistence::decode(&persistence::encode(&snapshot).unwrap()).unwrap();
    let key = &restored.panes.primary().navigation.draft_key;
    let DraftKey::Session { session } = key else {
        panic!("draft address missing")
    };
    assert_eq!(session.provider, None);
    assert!(uuid::Uuid::parse_str(&session.id).is_ok());
    assert_eq!(restored.input.drafts[key].text, "unfinished");
    assert!(!restored.input.drafts.contains_key(&old));
    let archived = &restored.lists.archived_scopes["other-host"];
    assert_eq!(
        archived.drafts[&archived.navigation.draft_key].text,
        "unfinished"
    );
    assert_ne!(&archived.navigation.draft_key, key);
    let again = persistence::decode(&persistence::encode(&restored).unwrap()).unwrap();
    assert_eq!(again.panes.primary().navigation.draft_key, *key);
}

#[test]
fn pre_cutover_pending_only_first_input_can_be_restored_after_navigation() {
    use agent_core::state::{DraftKey, Event, Intent, PendingSubmission, reduce};
    let input: agent_protocol::ids::ClientInputId = uuid::Uuid::new_v4().to_string().into();
    let mut original = Snapshot::default();
    Arc::make_mut(&mut original.sessions.pending_submissions).insert(
        input.clone(),
        Arc::new(PendingSubmission {
            new_draft_slot: None,
            cursor: None,
            sequence: 1,
            submitted_at_ms: None,
            draft_key: DraftKey::from("new:/orphan"),
            draft: Arc::new(Draft {
                text: "pending only".into(),
                ..Default::default()
            }),
            turn_id: None,
            after_item_id: None,
            accepted: false,
            delivery_unknown: true,
            queued: false,
        }),
    );
    let restored = persistence::decode(&persistence::encode(&original).unwrap()).unwrap();
    assert!(matches!(
        restored.sessions.pending_submissions[&input].draft_key,
        DraftKey::Session { .. }
    ));
    let failed = reduce(&restored, "", Event::SubmissionFailed(input)).0;
    let returned = reduce(
        &failed,
        "",
        Event::Intent(Intent::NewChat {
            cwd: "/orphan".into(),
        }),
    )
    .0;
    assert_eq!(
        returned.input.drafts[&returned.panes.primary().navigation.draft_key].text,
        "pending only"
    );
}
