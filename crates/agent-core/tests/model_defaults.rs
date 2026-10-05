use agent_core::{
    persistence,
    state::{
        DraftKey, Event, Intent, ModelDefaults, ModelDefaultsScope, Snapshot,
        operations::{LoadModels, Operation},
        reduce,
    },
    store::Store,
};
use agent_protocol::{
    models::{Model, ModelRef},
    operations::ModelPage,
};
use serde_json::json;
use std::sync::Arc;

fn catalog() -> Vec<Model> {
    serde_json::from_value(json!([
        {"id":"gpt", "model":{"instanceId":"codex","id":"shared"}, "displayName":"GPT", "isDefault":true, "capabilities":{"optionDescriptors":[{"id":"reasoningEffort","label":"Reasoning","type":"select","options":[{"id":"medium","label":"medium","isDefault":true},{"id":"high","label":"high","isDefault":false}],"currentValue":"medium"},{"id":"serviceTier","label":"Service Tier","type":"select","options":[{"id":"default","label":"Standard","isDefault":true},{"id":"priority","label":"priority","isDefault":false}],"currentValue":"default"}]}},
        {"id":"claude", "model":{"instanceId":"claude","id":"shared"}, "displayName":"Claude", "isDefault":true, "capabilities":{"optionDescriptors":[{"id":"reasoningEffort","label":"Reasoning","type":"select","options":[{"id":"medium","label":"medium","isDefault":true},{"id":"high","label":"high","isDefault":false}],"currentValue":"medium"},{"id":"serviceTier","label":"Service Tier","type":"select","options":[{"id":"default","label":"Standard","isDefault":true},{"id":"fast","label":"fast","isDefault":false}],"currentValue":"default"}]}}
    ]))
    .unwrap()
}

fn apply(snapshot: &Snapshot, intent: Intent) -> Snapshot {
    reduce(snapshot, Event::Intent(intent)).0
}

fn scoped_fixture() -> Snapshot {
    Snapshot {
        storage_scope: "vm:sessions".into(),
        models: Arc::new(catalog()),
        threads: Some(Arc::new(serde_json::from_value(json!({
            "data": [], "moreProjectIds":[], "hasMoreChats":false, "hasMoreProjects":false, "projects": [
                {"id":"outer", "name":"Outer", "roots":[{"path":"/repo/"}]},
                {"id":"inner", "name":"Inner", "roots":[{"path":"/repo/nested"}]},
                {"id":"windows", "name":"Windows", "roots":[{"path":"C:\\repo"}]}
            ]
        })).unwrap())),
        ..Default::default()
    }
}

proptest::proptest! {
    #[test]
    fn scoped_presets_inherit_isolate_and_restore_without_changing_existing_drafts(
        suffix in "[a-z]{1,12}",
        project_high in proptest::bool::ANY,
    ) {
        let environment = ModelDefaultsScope::Environment { id: "vm".into() };
        let project = ModelDefaultsScope::Project { environment: "vm".into(), project: "outer".into() };
        let inner = ModelDefaultsScope::Project { environment: "vm".into(), project: "inner".into() };
        let mut snapshot = apply(&scoped_fixture(), Intent::SelectDefaultEffort { scope: ModelDefaultsScope::Global, effort: Some("medium".into()) });
        snapshot = apply(&snapshot, Intent::SelectDefaultServiceTier { scope: environment.clone(), service_tier: Some("priority".into()) });
        let effort = if project_high { "high" } else { "medium" };
        snapshot = apply(&snapshot, Intent::SelectDefaultEffort { scope: project.clone(), effort: Some(effort.into()) });
        proptest::prop_assert!(snapshot.has_model_defaults_override(project.clone()));
        snapshot = apply(&snapshot, Intent::SelectDefaultServiceTier { scope: inner.clone(), service_tier: Some("default".into()) });
        for (cwd, expected_effort, tier) in [
            (format!("/repo/{suffix}"), effort, "priority"),
            (format!("/repo/nested/{suffix}"), "medium", "default"),
            (format!("/repo-sibling/{suffix}"), "medium", "priority"),
            (format!("C:\\repo\\{suffix}"), "medium", "priority"),
        ] {
            snapshot = apply(&snapshot, Intent::NewChat { cwd: cwd.clone() });
            let draft = &snapshot.drafts[&DraftKey::from(format!("new:{cwd}"))];
            proptest::prop_assert_eq!(draft.effort.as_deref(), Some(expected_effort));
            proptest::prop_assert_eq!(draft.service_tier.as_deref(), Some(tier));
        }
        let existing = snapshot.drafts.clone();
        let preferences = persistence::encode_client_preferences(&snapshot).unwrap();
        let bytes = persistence::encode(&snapshot).unwrap();
        snapshot = persistence::decode(&persistence::apply_client_preferences(&bytes, &preferences).unwrap()).unwrap();
        snapshot.threads = scoped_fixture().threads;
        snapshot.storage_scope = "vm:changed-session-namespace".into();
        proptest::prop_assert_eq!(&snapshot.drafts, &existing);
        proptest::prop_assert_eq!(snapshot.model_defaults(project.clone()).effort, Some(effort.into()));
        snapshot = apply(&snapshot, Intent::InheritModelDefaults { scope: project.clone() });
        proptest::prop_assert_eq!(&snapshot.drafts, &existing);
        proptest::prop_assert!(!snapshot.has_model_defaults_override(project.clone()));
        proptest::prop_assert_eq!(snapshot.model_defaults(project.clone()), snapshot.model_defaults(environment.clone()));
        snapshot.storage_scope = "different-vm".into();
        snapshot.models = Arc::new(catalog());
        let cwd = format!("/repo/other-{suffix}");
        snapshot = apply(&snapshot, Intent::NewChat { cwd: cwd.clone() });
        let draft = &snapshot.drafts[&DraftKey::from(format!("new:{cwd}"))];
        proptest::prop_assert_eq!(draft.effort.as_deref(), Some("medium"));
        proptest::prop_assert_eq!(draft.service_tier.as_deref(), Some("default"));
        proptest::prop_assert_eq!(snapshot.model_defaults(inner).service_tier, Some("default".into()));
    }
}

#[test]
fn scope_menus_target_real_defaults_without_cross_environment_projects() {
    let snapshot = scoped_fixture();
    let global = ModelDefaultsScope::Global;
    let environment = ModelDefaultsScope::Environment { id: "vm".into() };
    let choices = snapshot.model_project_scope_choices(global.clone());
    assert_eq!(choices[0].scope, global.clone());
    assert_eq!(
        choices[1].scope,
        ModelDefaultsScope::Project {
            environment: "vm".into(),
            project: "outer".into()
        }
    );
    let choices = snapshot.model_environment_scope_choices(global.clone());
    assert_eq!(choices[0].scope, environment.clone());
    assert_eq!(choices[1].scope, global);
    let choices = snapshot.model_project_scope_choices(environment.clone());
    assert_eq!(choices[0].scope, environment.clone());
    assert_eq!(
        snapshot.model_environment_scope_choices(choices[1].scope.clone())[0].scope,
        choices[1].scope
    );
    let offline = Snapshot::default();
    let projects = offline.model_project_scope_choices(environment.clone());
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].scope, ModelDefaultsScope::Global);
    let environments = offline.model_environment_scope_choices(environment);
    assert_eq!(environments.len(), 1);
    assert_eq!(environments[0].scope, ModelDefaultsScope::Global);
}

proptest::proptest! {
    #[test]
    fn defaults_survive_storage_and_apply_only_to_new_drafts(
        claude in proptest::bool::ANY,
        high in proptest::bool::ANY,
        fast in proptest::bool::ANY,
        catalog_first in proptest::bool::ANY,
    ) {
        let provider = if claude { "claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap() } else { "codex".parse::<agent_protocol::session::ProviderInstanceId>().unwrap() };
        let tier = if fast { if claude { "fast" } else { "priority" } } else { "default" };
        let model = ModelRef { instance_id: provider.clone(), id: "shared".into() };
        let effort = if high { "high" } else { "medium" };
        let snapshot = Snapshot { models: Arc::new(catalog()), ..Default::default() };
        let snapshot = apply(&snapshot, Intent::NewChat { cwd: "/old".into() });
        let old = snapshot.drafts[&DraftKey::from("new:/old")].clone();
        let snapshot = apply(&snapshot, Intent::SelectDefaultModel { scope: ModelDefaultsScope::Global, model: Some(model.clone()) });
        let snapshot = apply(&snapshot, Intent::SelectDefaultEffort { scope: ModelDefaultsScope::Global, effort: Some(effort.into()) });
        let snapshot = apply(&snapshot, Intent::SelectDefaultServiceTier { scope: ModelDefaultsScope::Global, service_tier: Some(tier.into()) });
        // Restore through the actual device-storage boundary (catalogs are ephemeral).
        let mut snapshot = persistence::decode(&persistence::encode(&snapshot).unwrap()).unwrap();
        let restored_defaults = snapshot.model_defaults.clone();
        if catalog_first { snapshot.models = Arc::new(catalog()); }
        snapshot = apply(&snapshot, Intent::NewChat { cwd: "/new".into() });
        if !catalog_first {
            LoadModels {}.apply(&mut snapshot, ModelPage { instances: Vec::new(), data: catalog(), next_cursor: None, provider_errors: None });
        }
        let new = &snapshot.drafts[&DraftKey::from("new:/new")];
        proptest::prop_assert_eq!(new.model.as_ref(), Some(&model));
        proptest::prop_assert_eq!(new.effort.as_deref(), Some(effort));
        proptest::prop_assert_eq!(new.service_tier.as_deref(), Some(tier));
        proptest::prop_assert_eq!(&snapshot.model_defaults, &restored_defaults);
        proptest::prop_assert_eq!(&snapshot.drafts[&DraftKey::from("new:/old")], &old);
        // Reopening an existing unsent draft must preserve its own choices.
        let snapshot = apply(&snapshot, Intent::SelectDefaultEffort { scope: ModelDefaultsScope::Global, effort: Some("medium".into()) });
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
            scope: ModelDefaultsScope::Global,
            effort: Some("high".into()),
        },
    );
    let snapshot = apply(
        &snapshot,
        Intent::SelectDefaultServiceTier {
            scope: ModelDefaultsScope::Global,
            service_tier: Some("priority".into()),
        },
    );
    let unchanged = apply(
        &snapshot,
        Intent::SelectDefaultModel {
            scope: ModelDefaultsScope::Global,
            model: None,
        },
    );
    assert_eq!(unchanged.model_defaults, snapshot.model_defaults);
    let controls = snapshot.default_model_controls(ModelDefaultsScope::Global);
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
        instance_id: "claude"
            .parse::<agent_protocol::session::ProviderInstanceId>()
            .unwrap(),
        id: "shared".into(),
    };
    let snapshot = apply(
        &snapshot,
        Intent::SelectDefaultModel {
            scope: ModelDefaultsScope::Global,
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
    let snapshot = apply(
        &snapshot,
        Intent::SelectDefaultModel {
            scope: ModelDefaultsScope::Global,
            model: None,
        },
    );
    assert_eq!(snapshot.model_defaults, ModelDefaults::default());
}

#[test]
fn new_drafts_normalize_unsupported_options_without_overwriting_preferences() {
    let snapshot = Snapshot {
        models: Arc::new(catalog()),
        model_defaults: ModelDefaults {
            model: Some(ModelRef {
                instance_id: "claude"
                    .parse::<agent_protocol::session::ProviderInstanceId>()
                    .unwrap(),
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
            instance_id: "claude"
                .parse::<agent_protocol::session::ProviderInstanceId>()
                .unwrap(),
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
            scope: ModelDefaultsScope::Global,
            effort: Some("high".into()),
        })
        .await
        .unwrap();
    assert!(updates.has_changed().unwrap());
    assert_eq!(
        updates.borrow_and_update().model_defaults.effort.as_deref(),
        Some("high")
    );
    let scope = ModelDefaultsScope::Environment { id: "vm".into() };
    store
        .dispatch(Intent::SelectDefaultEffort {
            scope: scope.clone(),
            effort: Some("medium".into()),
        })
        .await
        .unwrap();
    assert!(updates.has_changed().unwrap());
    assert_eq!(
        updates.borrow_and_update().scoped_model_defaults[&scope]
            .effort
            .as_deref(),
        Some("medium")
    );
    store.close().await.unwrap();
}
