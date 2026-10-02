use agent_core::{
    persistence,
    state::{
        DraftKey, Event, Intent, ModelDefaults, Snapshot,
        operations::{LoadModels, Operation},
        reduce,
    },
    store::Store,
};
use agent_protocol::{
    models::{Model, ModelRef},
    operations::ModelPage,
    session::ProviderKind,
};
use serde_json::json;
use std::sync::Arc;

fn catalog() -> Vec<Model> {
    serde_json::from_value(json!([
        {"id":"gpt","model":{"provider":"codex","id":"shared"},"displayName":"GPT",
         "isDefault":true,"defaultReasoningEffort":"medium",
         "supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"}],
         "serviceTiers":[{"id":"priority"}]},
        {"id":"claude","model":{"provider":"claude","id":"shared"},"displayName":"Claude",
         "isDefault":true,"defaultReasoningEffort":"medium",
         "supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"}],
         "serviceTiers":[{"id":"fast"}]}
    ]))
    .unwrap()
}

fn apply(snapshot: &Snapshot, intent: Intent) -> Snapshot {
    reduce(snapshot, Event::Intent(intent)).0
}

proptest::proptest! {
    #[test]
    fn defaults_survive_storage_and_apply_only_to_new_drafts(
        claude in proptest::bool::ANY,
        high in proptest::bool::ANY,
        fast in proptest::bool::ANY,
        catalog_first in proptest::bool::ANY,
    ) {
        let provider = if claude { ProviderKind::Claude } else { ProviderKind::Codex };
        let tier = if fast { if claude { "fast" } else { "priority" } } else { "default" };
        let model = ModelRef { provider, id: "shared".into() };
        let effort = if high { "high" } else { "medium" };
        let snapshot = Snapshot { models: Arc::new(catalog()), ..Default::default() };
        let snapshot = apply(&snapshot, Intent::NewChat { cwd: "/old".into() });
        let old = snapshot.drafts[&DraftKey::from("new:/old")].clone();
        let snapshot = apply(&snapshot, Intent::SelectDefaultModel { model: Some(model.clone()) });
        let snapshot = apply(&snapshot, Intent::SelectDefaultEffort { effort: Some(effort.into()) });
        let snapshot = apply(&snapshot, Intent::SelectDefaultServiceTier { service_tier: Some(tier.into()) });
        // Restore through the actual device-storage boundary (catalogs are ephemeral).
        let mut snapshot = persistence::decode(&persistence::encode(&snapshot).unwrap()).unwrap();
        let restored_defaults = snapshot.model_defaults.clone();
        if catalog_first { snapshot.models = Arc::new(catalog()); }
        snapshot = apply(&snapshot, Intent::NewChat { cwd: "/new".into() });
        if !catalog_first {
            LoadModels {}.apply(&mut snapshot, ModelPage { data: catalog(), next_cursor: None, provider_errors: None });
        }
        let new = &snapshot.drafts[&DraftKey::from("new:/new")];
        proptest::prop_assert_eq!(new.model.as_ref(), Some(&model));
        proptest::prop_assert_eq!(new.effort.as_deref(), Some(effort));
        proptest::prop_assert_eq!(new.service_tier.as_deref(), Some(tier));
        proptest::prop_assert_eq!(&snapshot.model_defaults, &restored_defaults);
        proptest::prop_assert_eq!(&snapshot.drafts[&DraftKey::from("new:/old")], &old);
        // Reopening an existing unsent draft must preserve its own choices.
        let snapshot = apply(&snapshot, Intent::SelectDefaultEffort { effort: Some("medium".into()) });
        let snapshot = apply(&snapshot, Intent::NewChat { cwd: "/new".into() });
        proptest::prop_assert_eq!(snapshot.drafts[&DraftKey::from("new:/new")].effort.as_deref(), Some(effort));
    }
}

#[test]
fn model_changes_reset_options_and_automatic_model_accepts_supported_options() {
    let snapshot = Snapshot {
        models: Arc::new(catalog()),
        ..Default::default()
    };
    let snapshot = apply(
        &snapshot,
        Intent::SelectDefaultEffort {
            effort: Some("high".into()),
        },
    );
    let snapshot = apply(
        &snapshot,
        Intent::SelectDefaultServiceTier {
            service_tier: Some("priority".into()),
        },
    );
    let unchanged = apply(&snapshot, Intent::SelectDefaultModel { model: None });
    assert_eq!(unchanged.model_defaults, snapshot.model_defaults);
    let controls = snapshot.default_model_controls();
    assert_eq!(controls.effort, "high");
    assert!(controls.fast);
    let snapshot = apply(
        &snapshot,
        Intent::NewChat {
            cwd: "/auto".into(),
        },
    );
    let draft = &snapshot.drafts[&DraftKey::from("new:/auto")];
    assert_eq!(draft.effort.as_deref(), Some("high"));
    assert_eq!(draft.service_tier.as_deref(), Some("priority"));
    let model = ModelRef {
        provider: ProviderKind::Claude,
        id: "shared".into(),
    };
    let snapshot = apply(
        &snapshot,
        Intent::SelectDefaultModel {
            model: Some(model.clone()),
        },
    );
    assert_eq!(
        snapshot.model_defaults,
        ModelDefaults {
            model: Some(model),
            ..Default::default()
        }
    );
    let snapshot = apply(&snapshot, Intent::SelectDefaultModel { model: None });
    assert_eq!(snapshot.model_defaults, ModelDefaults::default());
}

#[test]
fn new_drafts_normalize_unsupported_options_without_overwriting_preferences() {
    let snapshot = Snapshot {
        models: Arc::new(catalog()),
        model_defaults: ModelDefaults {
            model: Some(ModelRef {
                provider: ProviderKind::Claude,
                id: "retired".into(),
            }),
            effort: Some("invalid".into()),
            service_tier: Some("priority".into()),
        },
        ..Default::default()
    };
    let next = apply(
        &snapshot,
        Intent::NewChat {
            cwd: "/fallback".into(),
        },
    );
    let draft = &next.drafts[&DraftKey::from("new:/fallback")];
    assert_eq!(
        draft.model.as_ref().unwrap(),
        &ModelRef {
            provider: ProviderKind::Claude,
            id: "shared".into()
        }
    );
    assert_eq!(draft.effort.as_deref(), Some("medium"));
    assert_eq!(draft.service_tier.as_deref(), Some("default"));
    assert_eq!(next.model_defaults, snapshot.model_defaults);
}

#[tokio::test]
async fn preference_changes_publish_while_offline() {
    let store = Store::offline(Snapshot::default());
    let mut updates = store.subscribe();
    store
        .dispatch(Intent::SelectDefaultEffort {
            effort: Some("high".into()),
        })
        .await
        .unwrap();
    assert!(updates.has_changed().unwrap());
    assert_eq!(
        updates.borrow_and_update().model_defaults.effort.as_deref(),
        Some("high")
    );
    store.close().await.unwrap();
}
