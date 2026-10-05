//! Provider catalog presentation is shared by every native client.
use agent_protocol::{models::Model, providers::ProviderInstance};
use std::collections::HashMap;

/// T3 groups drivers in their first-seen order, then puts each driver's default
/// instance before its custom instances. Custom instances retain author order.
pub(crate) fn ordered_catalog(
    mut instances: Vec<ProviderInstance>,
    mut models: Vec<Model>,
) -> (Vec<ProviderInstance>, Vec<Model>) {
    let mut drivers = HashMap::new();
    for instance in &instances {
        let position = drivers.len();
        drivers
            .entry(instance.reference.driver.clone())
            .or_insert(position);
    }
    instances.sort_by_key(|instance| {
        (
            drivers[&instance.reference.driver],
            instance.reference.instance_id.as_str() != instance.reference.driver.as_str(),
        )
    });
    let positions: HashMap<_, _> = instances
        .iter()
        .enumerate()
        .map(|(position, instance)| (&instance.reference.instance_id, position))
        .collect();
    models.sort_by_key(|model| {
        positions
            .get(&model.model.instance_id)
            .copied()
            .unwrap_or(usize::MAX)
    });
    // Preference applies after every native page has arrived. Provider dispatch
    // IDs stay opaque, including the openai. prefix used by some installations.
    for instance in instances
        .iter()
        .filter(|instance| instance.reference.driver.as_str() == "codex")
    {
        let preferred = ["gpt-6-astra", "gpt-5.6-sol", "gpt-5.6-terra"]
            .iter()
            .find_map(|family| {
                models
                    .iter()
                    .find(|model| {
                        model.model.instance_id == instance.reference.instance_id
                            && !model.is_custom
                            && model
                                .model
                                .id
                                .strip_prefix("openai.")
                                .unwrap_or(&model.model.id)
                                == *family
                    })
                    .map(|model| model.model.id.clone())
            });
        if let Some(preferred) = preferred {
            for model in models
                .iter_mut()
                .filter(|model| model.model.instance_id == instance.reference.instance_id)
            {
                model.is_default = (model.model.id == preferred).then_some(true);
            }
        }
    }
    (instances, models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{models::ModelRef, providers::*};
    use proptest::prelude::*;

    fn instance(id: &str, driver: &str) -> ProviderInstance {
        ProviderInstance {
            reference: ProviderRef {
                instance_id: id.parse().unwrap(),
                driver: driver.parse().unwrap(),
            },
            display_name: id.into(),
            availability: ProviderAvailability::Ready,
            capabilities: Default::default(),
            requires_account: false,
        }
    }

    #[test]
    fn preferred_defaults_are_per_instance_and_never_promote_custom_models() {
        let model = |instance: &str, id: &str, custom: bool, default: bool| {
            serde_json::from_value(serde_json::json!({
                "id":id,"model":{"instanceId":instance,"id":id},"displayName":id,
                "capabilities":{"optionDescriptors":[]},"isCustom":custom,"isDefault":default
            }))
            .unwrap()
        };
        let instances = vec![
            instance("work", "codex"),
            instance("personal", "codex"),
            instance("claudeAgent", "claudeAgent"),
        ];
        let models = vec![
            model("work", "native-default", false, true),
            model("work", "gpt-5.6-sol", false, false),
            model("work", "openai.gpt-6-astra", false, false),
            model("personal", "gpt-6-astra", true, false),
            model("personal", "native-default", false, true),
            model("claudeAgent", "gpt-6-astra", false, false),
            model("claudeAgent", "native-default", false, true),
        ];
        let (instances, models) = ordered_catalog(instances, models);
        let defaults: Vec<_> = models
            .iter()
            .filter(|model| model.is_default == Some(true))
            .map(|model| (model.model.instance_id.as_str(), model.model.id.as_str()))
            .collect();
        assert_eq!(
            defaults,
            [
                ("work", "openai.gpt-6-astra"),
                ("personal", "native-default"),
                ("claudeAgent", "native-default")
            ]
        );
        let (_, again) = ordered_catalog(instances, models.clone());
        assert_eq!(models, again);
    }

    #[test]
    fn driver_groups_and_defaults_preserve_custom_order_and_model_choices() {
        let instances = vec![
            instance("work", "codex"),
            instance("claudeAgent", "claudeAgent"),
            instance("codex", "codex"),
            instance("personal", "codex"),
            instance("future", "FutureDriver"),
        ];
        let models: Vec<Model> = serde_json::from_value(serde_json::json!([
            {"id":"c", "model":{"instanceId":"claudeAgent","id":"c"}, "displayName":"C", "capabilities":{"optionDescriptors":[]}},
            {"id":"a", "model":{"instanceId":"codex","id":"a"}, "displayName":"A", "capabilities":{"optionDescriptors":[]}},
            {"id":"b", "model":{"instanceId":"codex","id":"b"}, "displayName":"B", "capabilities":{"optionDescriptors":[]}}
        ])).unwrap();
        let (_, reordered_models) = ordered_catalog(instances.clone(), models.clone());
        assert_eq!(
            reordered_models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        let (ordered, _) = ordered_catalog(instances, models);
        assert_eq!(
            ordered
                .iter()
                .map(|instance| instance.reference.instance_id.as_str())
                .collect::<Vec<_>>(),
            ["codex", "work", "personal", "claudeAgent", "future"]
        );
        assert_eq!(
            reordered_models[0].model,
            ModelRef {
                instance_id: "codex".parse().unwrap(),
                id: "a".into()
            }
        );
    }

    proptest! {
        #[test]
        fn sorting_keeps_every_instance_and_is_idempotent(ids in prop::collection::vec("[A-Za-z][A-Za-z0-9_-]{0,12}", 0..50)) {
            let instances: Vec<_> = ids.iter().enumerate().map(|(position, id)| instance(id, if position % 2 == 0 { "codex" } else { "FutureDriver" })).collect();
            let (ordered, _) = ordered_catalog(instances.clone(), Vec::new());
            prop_assert_eq!(ordered.len(), instances.len());
            let (again, _) = ordered_catalog(ordered.clone(), Vec::new());
            prop_assert_eq!(&ordered, &again);
            for instance in &instances {
                prop_assert_eq!(ordered.iter().filter(|other| *other == instance).count(), instances.iter().filter(|other| *other == instance).count());
            }
        }
    }
}
