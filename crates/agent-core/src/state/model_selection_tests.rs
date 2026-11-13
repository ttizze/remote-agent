use agent_core::{
    persistence,
    state::{
        Draft, DraftKey, Intent, Snapshot,
        operations::{ListAccounts, Operation, ReadThread},
    },
};
use agent_protocol::{
    models::{Model, ModelRef},
    operations::ModelPage,
    session::ProviderKind,
};
use serde_json::json;
use std::sync::Arc;

fn restart(snapshot: &Snapshot) -> Snapshot {
    use crate::client::persistence::{
        ClientStorage, DevicePreferences, Persistence, PreferenceKey, PreferenceStorage,
    };
    struct Storage {
        local: Vec<u8>,
        models: Vec<u8>,
    }
    impl ClientStorage for Storage {
        fn load(&self) -> Result<Vec<u8>, crate::client::AgentError> {
            Ok(self.local.clone())
        }
        fn save(&self, _: Vec<u8>) -> Result<(), crate::client::AgentError> {
            panic!("restoration must not write")
        }
    }
    impl PreferenceStorage for Storage {
        fn load(&self, key: PreferenceKey) -> Result<Vec<u8>, crate::client::AgentError> {
            Ok(match key {
                PreferenceKey::Models => self.models.clone(),
                PreferenceKey::Editor
                | PreferenceKey::Language
                | PreferenceKey::Composer
                | PreferenceKey::Contrast => Vec::new(),
            })
        }
        fn save(&self, _: PreferenceKey, _: Vec<u8>) -> Result<(), crate::client::AgentError> {
            panic!("restoration must not write")
        }
    }
    let storage = Arc::new(Storage {
        local: persistence::encode(snapshot).unwrap(),
        models: serde_json::to_vec(&snapshot.catalog.model_preferences).unwrap(),
    });
    let preferences = DevicePreferences::new(storage.clone());
    let persistence = Persistence::new(storage, preferences);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(persistence.restore(false, None))
        .unwrap()
}

fn catalog() -> Vec<Model> {
    serde_json::from_value(json!([
        {"id":"gpt","model":{"provider":"codex","id":"shared"},"displayName":"GPT",
         "isDefault":true, "settings": {"defaultReasoningEffort":"medium",
         "supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"}],
         "serviceTiers":[{"id":"priority","fast":true}]}},
        {"id":"claude","model":{"provider":"claude","id":"shared"},"displayName":"Claude",
         "isDefault":true, "settings": {"defaultReasoningEffort":"medium",
         "supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"}],
         "serviceTiers":[{"id":"fast","fast":true}]}}
    ]))
    .unwrap()
}

#[test]
fn explicit_codex_model_survives_selection_and_catalog_refresh() {
    let snapshot = Snapshot {
        catalog: crate::state::Catalog {
            models: Arc::new(catalog()),
            ..Default::default()
        },
        ..Default::default()
    };
    let key = snapshot.panes.primary().navigation.draft_key.clone();
    let model = ModelRef {
        provider: ProviderKind::Codex,
        id: "gpt-6.1-sol".into(),
    };
    assert_eq!(
        crate::presentation::view::Presentation::from_snapshot(
            Arc::new(snapshot.clone()),
            "".into()
        )
        .unwrap()
        .settings()
        .custom_model_choice(Some(ProviderKind::Codex), " gpt-6.1-sol "),
        Some(model.clone())
    );
    assert!(
        crate::presentation::view::Presentation::from_snapshot(
            Arc::new(snapshot.clone()),
            "".into()
        )
        .unwrap()
        .settings()
        .custom_model_choice(Some(ProviderKind::Claude), "gpt-6.1-sol")
        .is_none()
    );
    assert!(
        crate::presentation::view::Presentation::from_snapshot(
            Arc::new(snapshot.clone()),
            "".into()
        )
        .unwrap()
        .settings()
        .custom_model_choice(Some(ProviderKind::Codex), " ")
        .is_none()
    );
    assert!(
        crate::presentation::view::Presentation::from_snapshot(
            Arc::new(snapshot.clone()),
            "".into()
        )
        .unwrap()
        .settings()
        .custom_model_choice(Some(ProviderKind::Codex), "two words")
        .is_none()
    );
    assert!(
        crate::presentation::view::Presentation::from_snapshot(
            Arc::new(snapshot.clone()),
            "".into()
        )
        .unwrap()
        .settings()
        .custom_model_choice(Some(ProviderKind::Codex), "shared")
        .is_none()
    );
    let selected = apply(
        &snapshot,
        Intent::SelectModel {
            thread_id: key.clone(),
            model: model.clone(),
        },
    );
    assert_eq!(selected.input.drafts[&key].model, Some(model.clone()));
    let mut selected = selected;
    crate::test_models::apply_catalog(
        &mut selected,
        ModelPage {
            data: catalog(),
            next_cursor: None,
            provider_errors: None,
        },
    );
    assert_eq!(selected.input.drafts[&key].model, Some(model));
}

#[test]
fn chatgpt_catalog_repairs_unavailable_saved_model_but_api_keeps_explicit_ids() {
    for plan in ["pro", "plus", "API key"] {
        let mut snapshot = Snapshot {
            catalog: crate::state::Catalog {
                models: Arc::new(catalog()),
                ..Default::default()
            },
            ..Default::default()
        };
        let accounts = serde_json::from_value(json!({
            "accounts":[{"id":"desktop","provider":"codex","planType":plan}],
            "selected":{"codex":"desktop"}
        }))
        .unwrap();
        Arc::make_mut(&mut snapshot.catalog.account).accounts = Some(Arc::new(accounts));
        let key = snapshot.panes.primary().navigation.draft_key.clone();
        let unsupported = ModelRef {
            provider: ProviderKind::Codex,
            id: "gpt-6.1-sol".into(),
        };
        assert_eq!(
            crate::presentation::view::Presentation::from_snapshot(
                Arc::new(snapshot.clone()),
                "".into()
            )
            .unwrap()
            .settings()
            .custom_model_choice(Some(ProviderKind::Codex), "gpt-6.1-sol")
            .is_some(),
            plan == "API key"
        );
        snapshot = apply(
            &snapshot,
            Intent::SelectModel {
                thread_id: key.clone(),
                model: unsupported.clone(),
            },
        );
        crate::test_models::apply_catalog(
            &mut snapshot,
            ModelPage {
                data: catalog(),
                next_cursor: None,
                provider_errors: None,
            },
        );
        assert_eq!(
            snapshot.input.drafts[&key].model.as_ref().unwrap().id,
            if plan == "API key" {
                "gpt-6.1-sol"
            } else {
                "shared"
            }
        );
        if plan != "API key" {
            assert_eq!(
                snapshot.input.drafts[&key].effort.as_deref(),
                Some("medium")
            );
        }
        // A later native catalog can grant access: never blacklist the ID itself.
        let mut available = catalog();
        let mut added = available[0].clone();
        added.model = unsupported.clone();
        available.push(added);
        snapshot.catalog.models = Arc::new(available.clone());
        snapshot = apply(
            &snapshot,
            Intent::SelectModel {
                thread_id: key.clone(),
                model: unsupported.clone(),
            },
        );
        crate::test_models::apply_catalog(
            &mut snapshot,
            ModelPage {
                data: available,
                next_cursor: None,
                provider_errors: None,
            },
        );
        assert_eq!(snapshot.input.drafts[&key].model, Some(unsupported));
    }
}

#[rstest::rstest]
#[case::other_provider_failed("claude", "shared")]
#[case::selected_provider_failed("codex", "retired")]
fn catalog_failures_only_preserve_the_affected_providers_saved_model(
    #[case] failed: &str,
    #[case] expected: &str,
) {
    let mut snapshot = fixture();
    let mut accounts = snapshot
        .catalog
        .account
        .accounts
        .as_deref()
        .unwrap()
        .clone();
    accounts.accounts[0].plan_type = Some("pro".into());
    Arc::make_mut(&mut snapshot.catalog.account).accounts = Some(Arc::new(accounts));
    let key = snapshot.panes.primary().navigation.draft_key.clone();
    Arc::make_mut(&mut snapshot.input.drafts).insert(
        key.clone(),
        Arc::new(agent_core::state::Draft {
            model: Some(ModelRef {
                provider: ProviderKind::Codex,
                id: "retired".into(),
            }),
            ..Default::default()
        }),
    );
    crate::test_models::apply_catalog(
        &mut snapshot,
        ModelPage {
            data: catalog(),
            next_cursor: None,
            provider_errors: Some(
                serde_json::from_value(json!({(failed): {"message":"unavailable"}})).unwrap(),
            ),
        },
    );
    assert_eq!(
        snapshot.input.drafts[&key].model.as_ref().unwrap().id,
        expected
    );
    assert_eq!(
        snapshot
            .model_for_provider(key.clone(), ProviderKind::Codex)
            .unwrap()
            .id,
        expected
    );
}

fn apply(snapshot: &Snapshot, intent: Intent) -> Snapshot {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let store = crate::store::Store::offline(snapshot.clone(), None);
            // Native input controls observe accepted changes before awaiting Host effects.
            drop(store.dispatch(intent));
            let next = store.snapshot().as_ref().clone();
            store.close().await.unwrap();
            next
        })
}

#[test]
fn existing_conversation_can_choose_another_provider_without_losing_its_draft() {
    let id = agent_protocol::session::SessionRef {
        provider: Some(ProviderKind::Codex),
        id: "existing".into(),
    };
    let key = DraftKey::from(id.clone());
    let mut snapshot = Snapshot {
        catalog: crate::state::Catalog {
            models: Arc::new(catalog()),
            ..Default::default()
        },
        ..Default::default()
    };
    Arc::make_mut(&mut snapshot.sessions.conversations).insert(
        id.clone(),
        Arc::new(agent_protocol::models::Thread {
            id: Some(id.clone()),
            active_provider: Some(ProviderKind::Claude),
            ..Default::default()
        }),
    );
    assert_eq!(
        snapshot.model_provider_for_draft(key.clone()),
        Some(ProviderKind::Claude)
    );
    snapshot = apply(
        &snapshot,
        Intent::SetDraft {
            thread_id: key.clone(),
            draft: agent_core::state::Draft {
                text: "keep this requirement".into(),
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
            model: catalog()[1].model.clone(),
        },
    );
    assert_eq!(
        next.model_provider_for_draft(key.clone()),
        Some(ProviderKind::Claude)
    );
    assert_eq!(next.input.drafts[&key].text, "keep this requirement");
    assert_eq!(next.input.drafts[&key].effort.as_deref(), Some("medium"));
    assert_ne!(
        next.input.drafts[&key].service_tier.as_deref(),
        Some("priority")
    );
    assert!(next.sessions.conversations.contains_key(&id));
}

fn fixture() -> Snapshot {
    Snapshot {
        catalog: crate::state::Catalog {
            models: Arc::new(catalog()),
            account: Arc::new(agent_core::state::AccountState {
                accounts: Some(Arc::new(
                    serde_json::from_value(json!({
                        "accounts":[{"id":"a","provider":"codex"},{"id":"b","provider":"claude"}],
                        "selected":{"codex":"a","claude":"b"}
                    }))
                    .unwrap(),
                )),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn select(
    snapshot: &Snapshot,
    key: &DraftKey,
    provider: ProviderKind,
    effort: &str,
    tier: &str,
) -> Snapshot {
    let snapshot = apply(
        snapshot,
        Intent::SelectModel {
            thread_id: key.clone(),
            model: ModelRef {
                provider,
                id: "shared".into(),
            },
        },
    );
    let snapshot = apply(
        &snapshot,
        Intent::SelectEffort {
            thread_id: key.clone(),
            effort: effort.into(),
        },
    );
    apply(
        &snapshot,
        Intent::SelectServiceTier {
            thread_id: key.clone(),
            service_tier: tier.into(),
        },
    )
}

proptest::proptest! {
    #[test]
    fn last_explicit_selection_survives_restart_and_seeds_new_chats(
        claude in proptest::bool::ANY, high in proptest::bool::ANY, fast in proptest::bool::ANY,
        cwd in "/[a-z]{1,20}",
    ) {
        let provider = if claude { ProviderKind::Claude } else { ProviderKind::Codex };
        let effort = if high { "high" } else { "medium" };
        let tier = if !fast { "default" } else if claude { "fast" } else { "priority" };
        let snapshot = fixture();
        let key = snapshot.panes.primary().navigation.draft_key.clone();
        let snapshot = select(&snapshot, &key, provider, effort, tier);
        let preferences = snapshot.catalog.model_preferences.clone();
        let mut restored = restart(&snapshot);
        restored.catalog.models = snapshot.catalog.models.clone();
        restored.catalog.account = snapshot.catalog.account.clone();
        let next = apply(&restored, Intent::NewChat { cwd });
        let draft = &next.input.drafts[&next.panes.primary().navigation.draft_key];
        proptest::prop_assert_eq!(draft.model.as_ref(), Some(&ModelRef { provider, id: "shared".into() }));
        proptest::prop_assert_eq!(draft.effort.as_deref(), Some(effort));
        proptest::prop_assert_eq!(draft.service_tier.as_deref(), Some(tier));
        proptest::prop_assert!(!draft.model_settings_changed);
        proptest::prop_assert_eq!(next.catalog.model_preferences, preferences);
        proptest::prop_assert!(fixture().catalog.model_preferences.selections.is_empty());
    }
}

#[test]
fn switching_providers_restores_their_last_options_without_rewriting_other_drafts() {
    let snapshot = apply(
        &fixture(),
        Intent::NewChat {
            cwd: "/original".into(),
        },
    );
    let original = snapshot.panes.primary().navigation.draft_key.clone();
    let snapshot = select(
        &snapshot,
        &original,
        ProviderKind::Codex,
        "high",
        "priority",
    );
    let snapshot = apply(
        &snapshot,
        Intent::SetDraftText {
            thread_id: original.clone(),
            text: "unsent request".into(),
        },
    );
    let snapshot = apply(
        &snapshot,
        Intent::NewChat {
            cwd: "/another".into(),
        },
    );
    let key = snapshot.panes.primary().navigation.draft_key.clone();
    let snapshot = select(&snapshot, &key, ProviderKind::Claude, "medium", "fast");
    let codex = snapshot
        .model_for_provider(key.clone(), ProviderKind::Codex)
        .unwrap();
    let snapshot = apply(
        &snapshot,
        Intent::SelectModel {
            thread_id: key.clone(),
            model: codex,
        },
    );
    assert_eq!(snapshot.input.drafts[&key].effort.as_deref(), Some("high"));
    assert_eq!(
        snapshot.input.drafts[&key].service_tier.as_deref(),
        Some("priority")
    );
    let snapshot = apply(
        &snapshot,
        Intent::SelectEffort {
            thread_id: key.clone(),
            effort: "medium".into(),
        },
    );
    let reopened = apply(
        &snapshot,
        Intent::NewChat {
            cwd: "/original".into(),
        },
    );
    assert_eq!(reopened.input.drafts[&original].text, "unsent request");
    assert_eq!(
        reopened.input.drafts[&original].effort.as_deref(),
        Some("high")
    );
    let next = apply(
        &reopened,
        Intent::NewChat {
            cwd: "/third".into(),
        },
    );
    assert_eq!(
        next.input.drafts[&next.panes.primary().navigation.draft_key]
            .effort
            .as_deref(),
        Some("medium")
    );
    assert_eq!(
        next.catalog.model_preferences.selections[&ProviderKind::Claude]
            .service_tier
            .as_deref(),
        Some("fast")
    );
    let restored = restart(&next);
    assert_eq!(restored.input.drafts, next.input.drafts);
    assert_eq!(
        restored.catalog.model_preferences,
        next.catalog.model_preferences
    );
    assert_eq!(restored.input.drafts[&original].text, "unsent request");
}

#[test]
fn reading_a_conversation_and_refreshing_catalogs_do_not_remember_its_model() {
    let snapshot = fixture();
    let snapshot = select(
        &snapshot,
        &snapshot.panes.primary().navigation.draft_key,
        ProviderKind::Codex,
        "high",
        "priority",
    );
    let remembered = snapshot.catalog.model_preferences.clone();
    let session = agent_protocol::session::SessionRef {
        provider: Some(ProviderKind::Claude),
        id: "old".into(),
    };
    let mut snapshot = snapshot;
    ReadThread::open(session.clone())
        .prepare(&mut snapshot, "")
        .unwrap();
    crate::test_conversations::open(
        &mut snapshot,
        serde_json::from_value(json!({
            "thread":{"id":session,"cwd":"/old"},
            "model":{"provider":"claude","id":"shared"}, "effort":"medium", "serviceTier":"fast"
        }))
        .unwrap(),
    );
    assert_eq!(
        snapshot.input.drafts[&DraftKey::from(session)]
            .model
            .as_ref()
            .unwrap()
            .provider,
        ProviderKind::Claude
    );
    crate::test_models::apply_catalog(
        &mut snapshot,
        ModelPage {
            data: catalog(),
            next_cursor: None,
            provider_errors: None,
        },
    );
    assert_eq!(snapshot.catalog.model_preferences, remembered);
    let next = apply(
        &snapshot,
        Intent::NewChat {
            cwd: "/fresh".into(),
        },
    );
    assert_eq!(
        next.input.drafts[&next.panes.primary().navigation.draft_key]
            .model
            .as_ref()
            .unwrap()
            .provider,
        ProviderKind::Codex
    );
}

#[test]
fn explicit_changes_in_existing_chats_are_remembered_without_touching_other_chats() {
    let snapshot = fixture();
    let initial = snapshot.panes.primary().navigation.draft_key.clone();
    let snapshot = select(
        &snapshot,
        &initial,
        ProviderKind::Codex,
        "medium",
        "default",
    );
    let session = agent_protocol::session::SessionRef {
        provider: Some(ProviderKind::Claude),
        id: "existing".into(),
    };
    let snapshot = select(
        &snapshot,
        &session.into(),
        ProviderKind::Claude,
        "high",
        "fast",
    );
    assert_eq!(
        snapshot.input.drafts[&initial]
            .model
            .as_ref()
            .unwrap()
            .provider,
        ProviderKind::Codex
    );
    let next = apply(
        &snapshot,
        Intent::NewChat {
            cwd: "/fresh".into(),
        },
    );
    let draft = &next.input.drafts[&next.panes.primary().navigation.draft_key];
    assert_eq!(draft.model.as_ref().unwrap().provider, ProviderKind::Claude);
    assert_eq!(draft.effort.as_deref(), Some("high"));
    assert_eq!(draft.service_tier.as_deref(), Some("fast"));
}

#[test]
fn remembered_deepseek_route_waits_for_capabilities_without_losing_choices() {
    let model = ModelRef {
        provider: ProviderKind::DeepSeek,
        id: r#"["openrouter","deepseek/model"]"#.into(),
    };
    let snapshot = apply(
        &Snapshot::default(),
        Intent::SelectModel {
            thread_id: "local".into(),
            model: model.clone(),
        },
    );
    let snapshot = apply(
        &snapshot,
        Intent::SelectEffort {
            thread_id: "local".into(),
            effort: "max".into(),
        },
    );
    let mut snapshot = restart(&snapshot);
    snapshot = apply(
        &snapshot,
        Intent::NewChat {
            cwd: "/deepseek".into(),
        },
    );
    let key = snapshot.panes.primary().navigation.draft_key.clone();
    assert_eq!(snapshot.input.drafts[&key].model.as_ref(), Some(&model));
    assert_eq!(snapshot.input.drafts[&key].effort.as_deref(), Some("max"));
    crate::test_models::apply_catalog(
        &mut snapshot,
        ModelPage {
            data: vec![serde_json::from_value(json!({
        "id":"deepseek", "model":model,"displayName":"DeepSeek", "settings":{
            "defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low"}]
        }
    })).unwrap()],
            next_cursor: None,
            provider_errors: None,
        },
    );
    assert_eq!(snapshot.input.drafts[&key].effort.as_deref(), Some("low"));
    assert_eq!(
        snapshot.catalog.model_preferences.selections[&ProviderKind::DeepSeek]
            .effort
            .as_deref(),
        Some("max")
    );
}

#[test]
fn first_new_chat_waits_for_authentication_without_creating_a_preference() {
    for catalog_first in [false, true] {
        let mut snapshot = Snapshot::default();
        for load_catalog in [catalog_first, !catalog_first] {
            if load_catalog {
                crate::test_models::apply_catalog(
                    &mut snapshot,
                    ModelPage {
                        data: catalog(),
                        next_cursor: None,
                        provider_errors: None,
                    },
                );
            } else {
                snapshot = apply(
                    &snapshot,
                    Intent::NewChat {
                        cwd: "/late".into(),
                    },
                );
            }
        }
        assert!(
            snapshot.input.drafts[&snapshot.panes.primary().navigation.draft_key]
                .model
                .is_none()
        );
        ListAccounts {}.apply(
            &mut snapshot,
            "",
            fixture()
                .catalog
                .account
                .accounts
                .as_ref()
                .unwrap()
                .as_ref()
                .clone(),
        );
        assert!(
            snapshot.input.drafts[&snapshot.panes.primary().navigation.draft_key]
                .model
                .is_some()
        );
        assert!(snapshot.catalog.model_preferences.selections.is_empty());
    }
}

#[tokio::test]
async fn selecting_the_already_visible_model_publishes_the_remembered_choice() {
    let mut snapshot = fixture();
    let key = snapshot.panes.primary().navigation.draft_key.clone();
    Arc::make_mut(&mut snapshot.input.drafts).insert(
        key.clone(),
        Arc::new(Draft {
            model: Some(catalog()[0].model.clone()),
            effort: Some("high".into()),
            service_tier: Some("priority".into()),
            ..Default::default()
        }),
    );
    let store = agent_core::store::Store::offline(snapshot, None);
    let mut updates = store.subscribe();
    store
        .dispatch(Intent::SelectModel {
            thread_id: key,
            model: catalog()[0].model.clone(),
        })
        .await
        .unwrap();
    assert!(updates.has_changed().unwrap());
    assert_eq!(
        updates
            .borrow_and_update()
            .catalog
            .model_preferences
            .provider,
        Some(ProviderKind::Codex)
    );
    store.close().await.unwrap();
}

#[test]
fn selecting_another_providers_account_restores_its_settings_and_keeps_input() {
    use agent_core::state::operations::SelectAccountForDraft;
    let snapshot = fixture();
    let key = snapshot.panes.primary().navigation.draft_key.clone();
    let snapshot = select(&snapshot, &key, ProviderKind::Claude, "high", "fast");
    let snapshot = select(&snapshot, &key, ProviderKind::Codex, "medium", "default");
    let mut snapshot = apply(
        &snapshot,
        Intent::SetDraftText {
            thread_id: key.clone(),
            text: "keep this request".into(),
        },
    );
    let preferences = snapshot.catalog.model_preferences.clone();
    SelectAccountForDraft {
        provider: ProviderKind::Claude,
        id: "b".into(),
        thread_id: key.clone(),
    }
    .apply(
        &mut snapshot,
        "",
        (
            agent_protocol::operations::AccountSelection {
                provider: ProviderKind::Claude,
                selected_id: "b".into(),
                persistence_error: None,
            },
            Ok(ModelPage {
                data: catalog(),
                next_cursor: None,
                provider_errors: None,
            }),
        ),
    );
    assert_eq!(
        snapshot.input.drafts[&key].model.as_ref().unwrap().provider,
        ProviderKind::Claude
    );
    assert_eq!(snapshot.input.drafts[&key].text, "keep this request");
    assert_eq!(snapshot.input.drafts[&key].effort.as_deref(), Some("high"));
    assert_eq!(
        snapshot.input.drafts[&key].service_tier.as_deref(),
        Some("fast")
    );
    assert_eq!(snapshot.catalog.model_preferences, preferences);
}

#[test]
fn choosing_another_model_of_the_same_provider_uses_its_native_options() {
    let snapshot = fixture();
    let key = snapshot.panes.primary().navigation.draft_key.clone();
    let mut snapshot = select(&snapshot, &key, ProviderKind::Codex, "high", "priority");
    let mut alternate = catalog()[0].clone();
    alternate.model.id = "alternate".into();
    alternate.id = "alternate".into();
    let model = alternate.model.clone();
    Arc::make_mut(&mut snapshot.catalog.models).push(alternate);
    let snapshot = apply(
        &snapshot,
        Intent::SelectModel {
            thread_id: key.clone(),
            model: model.clone(),
        },
    );
    assert_eq!(
        snapshot.input.drafts[&key].effort.as_deref(),
        Some("medium")
    );
    assert_eq!(
        snapshot.input.drafts[&key].service_tier.as_deref(),
        Some("default")
    );
    let next = apply(
        &snapshot,
        Intent::NewChat {
            cwd: "/alternate".into(),
        },
    );
    assert_eq!(
        next.input.drafts[&next.panes.primary().navigation.draft_key]
            .model
            .as_ref(),
        Some(&model)
    );
    assert_eq!(
        next.input.drafts[&next.panes.primary().navigation.draft_key]
            .effort
            .as_deref(),
        Some("medium")
    );
}

#[test]
fn starting_a_new_chat_does_not_repair_other_conversations_from_the_catalog() {
    let snapshot = fixture();
    let snapshot = select(
        &snapshot,
        &snapshot.panes.primary().navigation.draft_key,
        ProviderKind::Codex,
        "medium",
        "default",
    );
    let key = DraftKey::from(agent_protocol::session::SessionRef {
        provider: Some(ProviderKind::Claude),
        id: "old-model".into(),
    });
    let previous = Arc::new(Draft {
        text: "keep the old conversation".into(),
        model: Some(ModelRef {
            provider: ProviderKind::Claude,
            id: "no-longer-listed".into(),
        }),
        effort: Some("high".into()),
        service_tier: Some("fast".into()),
        ..Default::default()
    });
    let mut snapshot = snapshot;
    Arc::make_mut(&mut snapshot.input.drafts).insert(key.clone(), previous.clone());
    let next = apply(
        &snapshot,
        Intent::NewChat {
            cwd: "/fresh".into(),
        },
    );
    assert_eq!(next.input.drafts[&key], previous);
}
