//! Prompt-controlled model options. The reducer owns all draft updates.
//! Adapted from T3 Tools Inc.'s MIT implementation; see third-party/T3-Code-LICENSE.
use agent_protocol::{
    model_prompt::{
        ULTRATHINK_PREFIX, apply_prompt_effort, contains_ultrathink, strip_ultrathink_prefix,
    },
    models::{
        ModelCapabilities, ModelOptionDescriptor, ModelOptionKind, ModelOptionSelection,
        ModelOptionValue, with_model_option,
    },
};

pub(crate) fn prompt_controlled_primary<'a>(
    capabilities: &'a ModelCapabilities,
    text: &str,
) -> Option<&'a ModelOptionDescriptor> {
    capabilities.primary_select()
        .filter(|descriptor| matches!(&descriptor.kind, ModelOptionKind::Select { prompt_injected_values, .. } if !prompt_injected_values.is_empty()))
        .filter(|_| contains_ultrathink(text))
}

pub(crate) fn select_option(
    capabilities: Option<&ModelCapabilities>,
    text: &str,
    selections: &[ModelOptionSelection],
    id: &str,
    value: Option<ModelOptionValue>,
) -> (String, Vec<ModelOptionSelection>) {
    let descriptor = capabilities.and_then(|capabilities| {
        capabilities
            .option_descriptors
            .iter()
            .find(|descriptor| descriptor.id == id)
    });
    if let Some(ModelOptionValue::String(value)) = &value {
        if value.is_empty() {
            return (text.into(), selections.to_vec());
        }
        if descriptor.is_some_and(|descriptor| matches!(&descriptor.kind, ModelOptionKind::Select { prompt_injected_values, .. } if prompt_injected_values.contains(value))) {
            let text = if text.trim().is_empty() {
                ULTRATHINK_PREFIX.into()
            } else {
                apply_prompt_effort(text, Some("ultrathink"))
            };
            return (text, selections.to_vec());
        }
    }
    let prompt_controlled = capabilities
        .and_then(|capabilities| prompt_controlled_primary(capabilities, text))
        .is_some_and(|descriptor| descriptor.id == id);
    if prompt_controlled && contains_ultrathink(strip_ultrathink_prefix(text)) {
        return (text.into(), selections.to_vec());
    }
    (
        if prompt_controlled {
            strip_ultrathink_prefix(text)
        } else {
            text
        }
        .into(),
        with_model_option(selections, id, value),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Draft, DraftKey, Event, Intent, Snapshot, reduce};
    use std::sync::Arc;

    fn snapshot(key: &DraftKey, text: &str) -> Snapshot {
        let model: agent_protocol::models::Model = serde_json::from_value(serde_json::json!({
            "id":"model", "model":{"instanceId":"custom","id":"model"}, "displayName":"Model",
            "capabilities":{"optionDescriptors":[
                {"id":"effort","label":"Effort","type":"select","currentValue":"medium","promptInjectedValues":["ultrathink"],"options":[
                    {"id":"medium","label":"Medium","isDefault":true}, {"id":"high","label":"High"}, {"id":"ultrathink","label":"Ultrathink"}]},
                {"id":"fastMode","label":"Fast","type":"boolean"},
                {"id":"contextWindow","label":"Context","type":"select","options":[{"id":"1m","label":"1M","isDefault":true}]}
            ]}
        })).unwrap();
        Snapshot {
            models: Arc::new(vec![model.clone()]),
            drafts: Arc::new(std::collections::BTreeMap::from([(
                key.clone(),
                Arc::new(Draft {
                    text: text.into(),
                    model: Some(model.model),
                    options: vec![ModelOptionSelection {
                        id: "fastMode".into(),
                        value: ModelOptionValue::Boolean(false),
                    }],
                    attachments: vec![crate::state::Attachment {
                        path: "image.png".into(),
                        name: "image.png".into(),
                        is_image: true,
                    }],
                    ..Default::default()
                }),
            )])),
            ..Default::default()
        }
    }

    fn select(snapshot: &Snapshot, key: &DraftKey, id: &str, value: ModelOptionValue) -> Snapshot {
        let (next, effects) = reduce(
            snapshot,
            Event::Intent(Intent::SelectModelOption {
                thread_id: key.clone(),
                id: id.into(),
                value: Some(value),
            }),
        );
        assert!(effects.is_empty());
        next
    }

    #[test]
    fn prompt_choice_edits_text_only_and_normal_choice_removes_only_our_prefix() {
        let key = DraftKey::from("draft");
        for text in ["investigate", "", "   "] {
            let original = snapshot(&key, text);
            let next = select(
                &original,
                &key,
                "effort",
                ModelOptionValue::String("ultrathink".into()),
            );
            assert_eq!(
                next.drafts[&key].text,
                if text.trim().is_empty() {
                    "Ultrathink:\n"
                } else {
                    "Ultrathink:\ninvestigate"
                }
            );
            assert_eq!(next.drafts[&key].options, original.drafts[&key].options);
            assert_eq!(
                next.drafts[&key].attachments,
                original.drafts[&key].attachments
            );
            assert_eq!(original.drafts[&key].text, text);
            let controls = next.model_option_controls(key.clone());
            let quick = next.model_quick_controls(key.clone());
            assert_eq!(quick.effort.as_ref().unwrap().value, controls[0].value);
            assert_eq!(
                quick.effort.as_ref().unwrap().value_label,
                controls[0].value_label
            );
            assert!(quick.effort.as_ref().unwrap().disabled_reason.is_none());
            assert_eq!(quick.effort_level, 3);
            assert_eq!(
                controls[0].value,
                Some(ModelOptionValue::String("ultrathink".into()))
            );
            assert_eq!(controls[0].value_label.as_deref(), Some("Ultrathink"));
            assert!(controls[0].disabled_reason.is_none());
            let high = select(
                &next,
                &key,
                "effort",
                ModelOptionValue::String("high".into()),
            );
            assert_eq!(high.drafts[&key].text, text.trim());
            assert_eq!(
                agent_protocol::models::model_option_string(&high.drafts[&key].options, "effort"),
                Some("high")
            );
            assert_eq!(
                agent_protocol::models::model_option_boolean(
                    &high.drafts[&key].options,
                    "fastMode"
                ),
                Some(false)
            );
        }
    }

    #[test]
    fn body_mentions_lock_only_primary_effort_and_other_options_remain_editable() {
        let key = DraftKey::from("draft");
        for text in [
            "please ULTRATHINK here",
            "Ultrathink:\nplease ultrathink here",
            " ultrathink:\nbody",
        ] {
            let original = snapshot(&key, text);
            let high = select(
                &original,
                &key,
                "effort",
                ModelOptionValue::String("high".into()),
            );
            assert_eq!(high.drafts[&key], original.drafts[&key]);
            assert!(
                high.model_option_controls(key.clone())[0]
                    .disabled_reason
                    .is_some()
            );
            assert!(
                high.model_quick_controls(key.clone())
                    .effort
                    .unwrap()
                    .disabled_reason
                    .is_some()
            );
            let fast = select(&high, &key, "fastMode", ModelOptionValue::Boolean(true));
            assert_eq!(fast.drafts[&key].text, text);
            assert_eq!(
                agent_protocol::models::model_option_boolean(
                    &fast.drafts[&key].options,
                    "fastMode"
                ),
                Some(true)
            );
            assert!(
                fast.model_option_controls(key.clone())[1]
                    .disabled_reason
                    .is_none()
            );
        }
    }

    #[test]
    fn native_commands_and_unavailable_or_unsupported_models_keep_normal_option_behavior() {
        let key = DraftKey::from("draft");
        let original = snapshot(&key, "/plugin:skill run");
        let next = select(
            &original,
            &key,
            "effort",
            ModelOptionValue::String("ultrathink".into()),
        );
        assert_eq!(next.drafts[&key], original.drafts[&key]);
        let mut unavailable = snapshot(&key, "ultrathink in text");
        unavailable.models = Arc::new(vec![]);
        let next = select(
            &unavailable,
            &key,
            "effort",
            ModelOptionValue::String("high".into()),
        );
        assert_eq!(next.drafts[&key].text, "ultrathink in text");
        assert_eq!(
            agent_protocol::models::model_option_string(&next.drafts[&key].options, "effort"),
            Some("high")
        );
        let mut unsupported = snapshot(&key, "ultrathink in text");
        Arc::make_mut(&mut unsupported.models)[0]
            .capabilities
            .option_descriptors[0]
            .kind = ModelOptionKind::Select {
            options: vec![],
            current_value: None,
            prompt_injected_values: vec![],
        };
        let next = select(
            &unsupported,
            &key,
            "effort",
            ModelOptionValue::String("high".into()),
        );
        assert_eq!(next.drafts[&key].text, "ultrathink in text");
        assert!(next.model_quick_controls(key.clone()).effort.is_none());
        assert_eq!(
            agent_protocol::models::model_option_string(&next.drafts[&key].options, "effort"),
            Some("high")
        );
    }

    proptest::proptest! {
        #[test]
        fn prompt_toggle_retains_draft_content_and_explicit_boolean(body in "[a-zA-Z0-9 ]{1,60}") {
            proptest::prop_assume!(!contains_ultrathink(&body));
            let key = DraftKey::from("draft");
            let original = snapshot(&key, &body);
            let prompt = select(&original, &key, "effort", ModelOptionValue::String("ultrathink".into()));
            let restored = select(&prompt, &key, "effort", ModelOptionValue::String("medium".into()));
            proptest::prop_assert_eq!(&restored.drafts[&key].text, body.trim());
            proptest::prop_assert_eq!(&restored.drafts[&key].attachments, &original.drafts[&key].attachments);
            proptest::prop_assert_eq!(agent_protocol::models::model_option_boolean(&restored.drafts[&key].options, "fastMode"), Some(false));
        }
    }
}
