use agent_core::{
    persistence,
    state::{
        DraftKey, Event, Intent, ModelDefaults, ModelDefaultsScope, ProviderModelDefaults,
        Snapshot,
        operations::{ListAccounts, LoadModels, Operation, SelectAccountForDraft},
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
         "serviceTiers":[{"id":"priority","fast":true}]},
        {"id":"claude","model":{"provider":"claude","id":"shared"},"displayName":"Claude",
         "isDefault":true,"defaultReasoningEffort":"medium",
         "supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"}],
         "serviceTiers":[{"id":"fast","fast":true}]}
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
        account: Arc::new(agent_core::state::AccountState {
            accounts: Some(Arc::new(serde_json::from_value(json!({
                "accounts": [{"id":"a","provider":"codex"},{"id":"b","provider":"claude"}],
                "selected": {"codex":"a","claude":"b"}
            })).unwrap())),
            ..Default::default()
        }),
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
        let mut snapshot = apply(&scoped_fixture(), Intent::SelectDefaultEffort { provider: ProviderKind::Codex, scope: ModelDefaultsScope::Global, effort: Some("medium".into()) });
        snapshot = apply(&snapshot, Intent::SelectDefaultServiceTier { provider: ProviderKind::Codex, scope: environment.clone(), service_tier: Some("priority".into()) });
        let effort = if project_high { "high" } else { "medium" };
        snapshot = apply(&snapshot, Intent::SelectDefaultEffort { provider: ProviderKind::Codex, scope: project.clone(), effort: Some(effort.into()) });
        proptest::prop_assert!(snapshot.has_model_defaults_override(project.clone()));
        snapshot = apply(&snapshot, Intent::SelectDefaultServiceTier { provider: ProviderKind::Codex, scope: inner.clone(), service_tier: Some("default".into()) });
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
        let preferences = persistence::encode_model_preferences(&snapshot).unwrap();
        let bytes = persistence::encode(&snapshot).unwrap();
        snapshot = persistence::decode(&persistence::apply_model_preferences(&bytes, &preferences).unwrap()).unwrap();
        snapshot.threads = scoped_fixture().threads;
        snapshot.storage_scope = "vm:changed-session-namespace".into();
        proptest::prop_assert_eq!(&snapshot.drafts, &existing);
        proptest::prop_assert_eq!(snapshot.provider_model_defaults(project.clone(), ProviderKind::Codex).effort, Some(effort.into()));
        snapshot = apply(&snapshot, Intent::InheritModelDefaults { scope: project.clone() });
        proptest::prop_assert_eq!(&snapshot.drafts, &existing);
        proptest::prop_assert!(!snapshot.has_model_defaults_override(project.clone()));
        proptest::prop_assert_eq!(snapshot.model_defaults(project.clone()), snapshot.model_defaults(environment.clone()));
        snapshot.storage_scope = "different-vm".into();
        snapshot.models = Arc::new(catalog());
        snapshot.account = scoped_fixture().account;
        let cwd = format!("/repo/other-{suffix}");
        snapshot = apply(&snapshot, Intent::NewChat { cwd: cwd.clone() });
        let draft = &snapshot.drafts[&DraftKey::from(format!("new:{cwd}"))];
        proptest::prop_assert_eq!(draft.effort.as_deref(), Some("medium"));
        proptest::prop_assert_eq!(draft.service_tier.as_deref(), Some("default"));
        proptest::prop_assert_eq!(snapshot.provider_model_defaults(inner, ProviderKind::Codex).service_tier, Some("default".into()));
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
        let provider = if claude { ProviderKind::Claude } else { ProviderKind::Codex };
        let tier = if fast { if claude { "fast" } else { "priority" } } else { "default" };
        let model = ModelRef { provider, id: "shared".into() };
        let effort = if high { "high" } else { "medium" };
        let snapshot = Snapshot { models: Arc::new(catalog()), ..Default::default() };
        let snapshot = apply(&snapshot, Intent::NewChat { cwd: "/old".into() });
        let old = snapshot.drafts[&DraftKey::from("new:/old")].clone();
        let snapshot = apply(&snapshot, Intent::SelectNewChatModel { scope: ModelDefaultsScope::Global, model: Some(model.clone()) });
        let snapshot = apply(&snapshot, Intent::SelectDefaultModel { provider, scope: ModelDefaultsScope::Global, model: Some(model.clone()) });
        let snapshot = apply(&snapshot, Intent::SelectDefaultEffort { provider, scope: ModelDefaultsScope::Global, effort: Some(effort.into()) });
        let snapshot = apply(&snapshot, Intent::SelectDefaultServiceTier { provider, scope: ModelDefaultsScope::Global, service_tier: Some(tier.into()) });
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
        let snapshot = apply(&snapshot, Intent::SelectDefaultEffort { provider, scope: ModelDefaultsScope::Global, effort: Some("medium".into()) });
        let snapshot = apply(&snapshot, Intent::NewChat { cwd: "/new".into() });
        proptest::prop_assert_eq!(snapshot.drafts[&DraftKey::from("new:/new")].effort.as_deref(), Some(effort));
    }
}

#[test]
fn model_changes_reset_options_and_automatic_model_accepts_supported_options() {
    let snapshot = scoped_fixture();
    let snapshot = apply(
        &snapshot,
        Intent::SelectDefaultEffort {
            provider: ProviderKind::Codex,
            scope: ModelDefaultsScope::Global,
            effort: Some("high".into()),
        },
    );
    let snapshot = apply(
        &snapshot,
        Intent::SelectDefaultServiceTier {
            provider: ProviderKind::Codex,
            scope: ModelDefaultsScope::Global,
            service_tier: Some("priority".into()),
        },
    );
    let unchanged = apply(
        &snapshot,
        Intent::SelectDefaultModel {
            provider: ProviderKind::Codex,
            scope: ModelDefaultsScope::Global,
            model: None,
        },
    );
    assert_eq!(unchanged.model_defaults, snapshot.model_defaults);
    let controls = snapshot.default_model_controls(ModelDefaultsScope::Global, ProviderKind::Codex);
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
            provider: ProviderKind::Claude,
            scope: ModelDefaultsScope::Global,
            model: Some(model.clone()),
        },
    );
    assert_eq!(
        snapshot.provider_model_defaults(ModelDefaultsScope::Global, ProviderKind::Claude),
        ProviderModelDefaults {
            model: Some(model),
            ..Default::default()
        }
    );
    let snapshot = apply(
        &snapshot,
        Intent::SelectDefaultModel {
            provider: ProviderKind::Claude,
            scope: ModelDefaultsScope::Global,
            model: None,
        },
    );
    assert_eq!(
        snapshot.provider_model_defaults(ModelDefaultsScope::Global, ProviderKind::Claude),
        ProviderModelDefaults::default()
    );
    assert_eq!(
        snapshot
            .provider_model_defaults(ModelDefaultsScope::Global, ProviderKind::Codex)
            .effort
            .as_deref(),
        Some("high")
    );
}

#[test]
fn new_drafts_normalize_unsupported_options_without_overwriting_preferences() {
    let snapshot = Snapshot {
        models: Arc::new(catalog()),
        model_defaults: ModelDefaults {
            new_chat_model: Some(ModelRef {
                provider: ProviderKind::Claude,
                id: "retired".into(),
            }),
            providers: [(
                ProviderKind::Claude,
                ProviderModelDefaults {
                    model: Some(ModelRef {
                        provider: ProviderKind::Claude,
                        id: "retired".into(),
                    }),
                    effort: Some("invalid".into()),
                    service_tier: Some("priority".into()),
                },
            )]
            .into(),
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
            provider: ProviderKind::Codex,
            scope: ModelDefaultsScope::Global,
            effort: Some("high".into()),
        })
        .await
        .unwrap();
    assert!(updates.has_changed().unwrap());
    assert_eq!(
        updates
            .borrow_and_update()
            .provider_model_defaults(ModelDefaultsScope::Global, ProviderKind::Codex)
            .effort
            .as_deref(),
        Some("high")
    );
    let scope = ModelDefaultsScope::Environment { id: "vm".into() };
    store
        .dispatch(Intent::SelectDefaultEffort {
            provider: ProviderKind::Codex,
            scope: scope.clone(),
            effort: Some("medium".into()),
        })
        .await
        .unwrap();
    assert!(updates.has_changed().unwrap());
    assert_eq!(
        updates.borrow_and_update().scoped_model_defaults[&scope].providers[&ProviderKind::Codex]
            .effort
            .as_deref(),
        Some("medium")
    );
    store.close().await.unwrap();
}

proptest::proptest! {
    #[test]
    fn provider_presets_and_new_chat_model_remain_independent(
        start_claude in proptest::bool::ANY,
        reverse_catalog in proptest::bool::ANY,
        codex_high in proptest::bool::ANY,
        claude_high in proptest::bool::ANY,
    ) {
        let mut models = catalog();
        for mut model in catalog() {
            model.model.id = "alternate".into();
            model.id = format!("{:?}-alternate", model.model.provider);
            model.is_default = Some(false);
            models.push(model);
        }
        if reverse_catalog { models.reverse(); }
        let mut snapshot = scoped_fixture();
        snapshot.models = Arc::new(models.clone());
        for (provider, high, tier) in [
            (ProviderKind::Codex, codex_high, "priority"),
            (ProviderKind::Claude, claude_high, "fast"),
        ] {
            snapshot = apply(&snapshot, Intent::SelectDefaultModel {
                scope: ModelDefaultsScope::Global, provider,
                model: Some(ModelRef { provider, id: "alternate".into() }),
            });
            snapshot = apply(&snapshot, Intent::SelectDefaultEffort {
                scope: ModelDefaultsScope::Global, provider, effort: Some(if high { "high" } else { "medium" }.into()),
            });
            snapshot = apply(&snapshot, Intent::SelectDefaultServiceTier {
                scope: ModelDefaultsScope::Global, provider, service_tier: Some(tier.into()),
            });
        }
        let provider = if start_claude { ProviderKind::Claude } else { ProviderKind::Codex };
        let starting_model = ModelRef { provider, id: "shared".into() };
        let presets = snapshot.model_defaults.providers.clone();
        snapshot = apply(&snapshot, Intent::SelectNewChatModel {
            scope: ModelDefaultsScope::Global, model: Some(starting_model.clone()),
        });
        proptest::prop_assert_eq!(&snapshot.model_defaults.providers, &presets);
        let preferences = persistence::encode_model_preferences(&snapshot).unwrap();
        snapshot = persistence::decode(&persistence::apply_model_preferences(&[], &preferences).unwrap()).unwrap();
        let automatic_provider = if reverse_catalog { ProviderKind::Claude } else { ProviderKind::Codex };
        snapshot.models = Arc::new(models);
        snapshot.account = scoped_fixture().account;
        snapshot = apply(&snapshot, Intent::NewChat { cwd: "/independent".into() });
        let key = snapshot.navigation.draft_key.clone();
        proptest::prop_assert_eq!(snapshot.drafts[&key].model.as_ref(), Some(&starting_model));
        for (provider, high, tier) in [
            (if start_claude { ProviderKind::Codex } else { ProviderKind::Claude }, if start_claude { codex_high } else { claude_high }, if start_claude { "priority" } else { "fast" }),
            (provider, if start_claude { claude_high } else { codex_high }, if start_claude { "fast" } else { "priority" }),
        ] {
            let expected = ModelRef { provider, id: "alternate".into() };
            let choice = snapshot.model_for_provider(key.clone(), provider).unwrap();
            proptest::prop_assert_eq!(&choice, &expected);
            snapshot = apply(&snapshot, Intent::SelectModel { thread_id: key.clone(), model: choice });
            proptest::prop_assert_eq!(snapshot.drafts[&key].effort.as_deref(), Some(if high { "high" } else { "medium" }));
            proptest::prop_assert_eq!(snapshot.drafts[&key].service_tier.as_deref(), Some(tier));
        }
        proptest::prop_assert_eq!(&snapshot.model_defaults.providers, &presets);
        proptest::prop_assert_eq!(&snapshot.model_defaults.new_chat_model, &Some(starting_model));
        snapshot = apply(&snapshot, Intent::SelectNewChatModel { scope: ModelDefaultsScope::Global, model: None });
        snapshot = apply(&snapshot, Intent::NewChat { cwd: "/automatic".into() });
        proptest::prop_assert_eq!(snapshot.drafts[&snapshot.navigation.draft_key].model.as_ref(), Some(&ModelRef { provider: automatic_provider, id: "alternate".into() }));
    }
}

#[test]
fn mismatched_provider_model_is_rejected_and_automatic_controls_stay_separate() {
    let snapshot = Snapshot {
        models: Arc::new(catalog()),
        ..Default::default()
    };
    let next = apply(
        &snapshot,
        Intent::SelectDefaultModel {
            scope: ModelDefaultsScope::Global,
            provider: ProviderKind::Codex,
            model: Some(ModelRef {
                provider: ProviderKind::Claude,
                id: "shared".into(),
            }),
        },
    );
    assert_eq!(next.model_defaults, snapshot.model_defaults);
    for (provider, tier) in [
        (ProviderKind::Codex, "priority"),
        (ProviderKind::Claude, "fast"),
    ] {
        let model = snapshot.model_for_provider("new".into(), provider).unwrap();
        assert_eq!(model.provider, provider);
        assert_eq!(
            snapshot
                .default_model_controls(ModelDefaultsScope::Global, provider)
                .fast_service_tier
                .as_deref(),
            Some(tier)
        );
    }
}

#[test]
fn scoped_new_chat_model_inherits_provider_presets_without_changing_them() {
    let global = ModelDefaultsScope::Global;
    let environment = ModelDefaultsScope::Environment { id: "vm".into() };
    let project = ModelDefaultsScope::Project {
        environment: "vm".into(),
        project: "outer".into(),
    };
    let mut snapshot = scoped_fixture();
    for provider in [ProviderKind::Codex, ProviderKind::Claude] {
        snapshot = apply(
            &snapshot,
            Intent::SelectDefaultModel {
                scope: global.clone(),
                provider,
                model: Some(ModelRef {
                    provider,
                    id: "shared".into(),
                }),
            },
        );
        snapshot = apply(
            &snapshot,
            Intent::SelectDefaultEffort {
                scope: global.clone(),
                provider,
                effort: Some("high".into()),
            },
        );
    }
    snapshot = apply(
        &snapshot,
        Intent::SelectNewChatModel {
            scope: environment.clone(),
            model: Some(ModelRef {
                provider: ProviderKind::Codex,
                id: "shared".into(),
            }),
        },
    );
    snapshot = apply(
        &snapshot,
        Intent::SelectNewChatModel {
            scope: project.clone(),
            model: Some(ModelRef {
                provider: ProviderKind::Claude,
                id: "shared".into(),
            }),
        },
    );
    assert_eq!(
        snapshot.model_defaults(project.clone()).providers,
        snapshot.model_defaults(global).providers
    );
    for (cwd, provider) in [
        ("/repo/nested-child", ProviderKind::Claude),
        ("/other", ProviderKind::Codex),
    ] {
        snapshot = apply(&snapshot, Intent::NewChat { cwd: cwd.into() });
        let draft = &snapshot.drafts[&snapshot.navigation.draft_key];
        assert_eq!(draft.model.as_ref().unwrap().provider, provider);
        assert_eq!(draft.effort.as_deref(), Some("high"));
    }
    snapshot = apply(
        &snapshot,
        Intent::InheritModelDefaults {
            scope: project.clone(),
        },
    );
    assert_eq!(
        snapshot.model_defaults(project),
        snapshot.model_defaults(environment)
    );
}

#[test]
fn unavailable_provider_preset_falls_back_only_after_a_complete_catalog() {
    let selected = ModelRef {
        provider: ProviderKind::Codex,
        id: "retired".into(),
    };
    let mut snapshot = scoped_fixture();
    snapshot = apply(
        &snapshot,
        Intent::SelectDefaultModel {
            scope: ModelDefaultsScope::Global,
            provider: ProviderKind::Codex,
            model: Some(selected.clone()),
        },
    );
    snapshot = apply(
        &snapshot,
        Intent::SelectDefaultEffort {
            scope: ModelDefaultsScope::Global,
            provider: ProviderKind::Codex,
            effort: Some("high".into()),
        },
    );
    snapshot = apply(
        &snapshot,
        Intent::SelectDefaultServiceTier {
            scope: ModelDefaultsScope::Global,
            provider: ProviderKind::Codex,
            service_tier: Some("priority".into()),
        },
    );
    let fallback = snapshot
        .model_for_provider("new".into(), ProviderKind::Codex)
        .unwrap();
    assert_eq!(fallback.id, "shared");
    let controls = snapshot.default_model_controls(ModelDefaultsScope::Global, ProviderKind::Codex);
    assert_eq!(controls.effort, "medium");
    assert!(!controls.fast);
    snapshot.model_errors =
        Arc::new(serde_json::from_value(json!({"codex":{"message":"unavailable"}})).unwrap());
    assert_eq!(
        snapshot.model_for_provider("new".into(), ProviderKind::Codex),
        Some(selected)
    );
    assert!(
        snapshot
            .default_model_controls(ModelDefaultsScope::Global, ProviderKind::Codex)
            .efforts
            .is_empty()
    );
}

#[test]
fn automatic_new_chat_waits_for_accounts_and_uses_an_authenticated_provider() {
    let mut snapshot = apply(
        &Snapshot::default(),
        Intent::NewChat {
            cwd: "/late".into(),
        },
    );
    let mut models = catalog();
    models.reverse();
    LoadModels {}.apply(
        &mut snapshot,
        ModelPage {
            data: models,
            next_cursor: None,
            provider_errors: None,
        },
    );
    assert!(
        snapshot
            .model_provider_for_draft(snapshot.navigation.draft_key.clone())
            .is_none()
    );
    ListAccounts {}.apply(
        &mut snapshot,
        serde_json::from_value(json!({
            "accounts": [{"id":"b","provider":"claude"}], "selected":{"claude":"b"}
        }))
        .unwrap(),
    );
    assert_eq!(
        snapshot.drafts[&snapshot.navigation.draft_key]
            .model
            .as_ref()
            .unwrap()
            .provider,
        ProviderKind::Claude
    );
}

#[test]
fn selecting_another_providers_account_applies_its_preset_and_preserves_draft_content() {
    let mut snapshot = scoped_fixture();
    snapshot = apply(
        &snapshot,
        Intent::SelectDefaultModel {
            scope: ModelDefaultsScope::Global,
            provider: ProviderKind::Claude,
            model: Some(ModelRef {
                provider: ProviderKind::Claude,
                id: "shared".into(),
            }),
        },
    );
    snapshot = apply(
        &snapshot,
        Intent::SelectDefaultEffort {
            scope: ModelDefaultsScope::Global,
            provider: ProviderKind::Claude,
            effort: Some("high".into()),
        },
    );
    snapshot = apply(
        &snapshot,
        Intent::SelectDefaultServiceTier {
            scope: ModelDefaultsScope::Global,
            provider: ProviderKind::Claude,
            service_tier: Some("fast".into()),
        },
    );
    snapshot = apply(
        &snapshot,
        Intent::NewChat {
            cwd: "/account".into(),
        },
    );
    let key = snapshot.navigation.draft_key.clone();
    snapshot = apply(
        &snapshot,
        Intent::SetDraftText {
            thread_id: key.clone(),
            text: "keep this draft".into(),
        },
    );
    SelectAccountForDraft {
        provider: ProviderKind::Claude,
        id: "claude-account".into(),
        thread_id: key.clone(),
    }
    .apply(
        &mut snapshot,
        (
            agent_protocol::operations::AccountSelection {
                provider: ProviderKind::Claude,
                selected_id: "claude-account".into(),
                persistence_error: None,
            },
            Ok(ModelPage {
                data: catalog(),
                next_cursor: None,
                provider_errors: None,
            }),
        ),
    );
    let draft = &snapshot.drafts[&key];
    assert_eq!(draft.model.as_ref().unwrap().provider, ProviderKind::Claude);
    assert_eq!(draft.effort.as_deref(), Some("high"));
    assert_eq!(draft.service_tier.as_deref(), Some("fast"));
    assert_eq!(draft.text, "keep this draft");
    assert_eq!(snapshot.model_defaults.new_chat_model, None);
}

#[test]
fn manual_model_changes_in_existing_conversations_keep_native_option_defaults() {
    let mut snapshot = scoped_fixture();
    let mut alternate = catalog()[0].clone();
    alternate.model.id = "alternate".into();
    alternate.id = "alternate".into();
    alternate.is_default = Some(false);
    let model = alternate.model.clone();
    Arc::make_mut(&mut snapshot.models).push(alternate);
    snapshot = apply(
        &snapshot,
        Intent::SelectDefaultEffort {
            scope: ModelDefaultsScope::Global,
            provider: ProviderKind::Codex,
            effort: Some("high".into()),
        },
    );
    snapshot = apply(
        &snapshot,
        Intent::SelectDefaultServiceTier {
            scope: ModelDefaultsScope::Global,
            provider: ProviderKind::Codex,
            service_tier: Some("priority".into()),
        },
    );
    let key: DraftKey = agent_protocol::session::SessionRef {
        provider: ProviderKind::Codex,
        id: "existing".into(),
    }
    .into();
    snapshot = apply(
        &snapshot,
        Intent::SetDraft {
            thread_id: key.clone(),
            draft: agent_core::state::Draft {
                text: "keep this draft".into(),
                model: Some(catalog()[0].model.clone()),
                effort: Some("high".into()),
                service_tier: Some("priority".into()),
                ..Default::default()
            },
        },
    );
    let next = apply(
        &snapshot,
        Intent::SelectModel {
            thread_id: key.clone(),
            model,
        },
    );
    assert_eq!(next.drafts[&key].effort.as_deref(), Some("medium"));
    assert_eq!(next.drafts[&key].service_tier.as_deref(), Some("default"));
    assert_eq!(next.drafts[&key].text, "keep this draft");
    assert_eq!(next.model_defaults, snapshot.model_defaults);
}

#[test]
fn initial_provider_choice_uses_preset_options_for_an_unassigned_local_draft() {
    let mut snapshot = scoped_fixture();
    snapshot = apply(
        &snapshot,
        Intent::SelectDefaultEffort {
            scope: ModelDefaultsScope::Global,
            provider: ProviderKind::Codex,
            effort: Some("high".into()),
        },
    );
    snapshot = apply(
        &snapshot,
        Intent::SelectDefaultServiceTier {
            scope: ModelDefaultsScope::Global,
            provider: ProviderKind::Codex,
            service_tier: Some("priority".into()),
        },
    );
    let key = DraftKey::from("local");
    snapshot = apply(
        &snapshot,
        Intent::SelectModel {
            thread_id: key.clone(),
            model: catalog()[0].model.clone(),
        },
    );
    assert_eq!(snapshot.drafts[&key].effort.as_deref(), Some("high"));
    assert_eq!(
        snapshot.drafts[&key].service_tier.as_deref(),
        Some("priority")
    );
}
