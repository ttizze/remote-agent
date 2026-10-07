//! The order models appear in a picker: favorites first when grouped, then
//! the user's order or the instance order, then the catalogue's order.
use std::collections::{HashMap, HashSet};

/// A favorite model, keyed by instance and slug.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct FavoriteModel {
    pub instance_id: String,
    pub model: String,
}

pub fn provider_model_key(instance_id: &str, slug: &str) -> String {
    format!("{instance_id}:{slug}")
}

/// Adds the model to the favorites, or removes it when it is one.
pub fn toggle_favorite(
    favorites: &[FavoriteModel],
    instance_id: &str,
    model: &str,
) -> Vec<FavoriteModel> {
    let mut next = favorites.to_vec();
    match next
        .iter()
        .position(|favorite| favorite.instance_id == instance_id && favorite.model == model)
    {
        Some(index) => {
            next.remove(index);
        }
        None => next.push(FavoriteModel {
            instance_id: instance_id.into(),
            model: model.into(),
        }),
    }
    next
}

fn rank<'a>(values: impl IntoIterator<Item = &'a str>) -> HashMap<&'a str, usize> {
    let mut ranks = HashMap::new();
    for (index, value) in values.into_iter().enumerate() {
        ranks.insert(value, index);
    }
    ranks
}

/// One instance's models: favorites first when grouped, then `model_order`,
/// then the catalogue order.
pub fn sort_models_for_provider_instance<T>(
    models: Vec<T>,
    slug: impl Fn(&T) -> &str,
    model_order: &[String],
    favorite_models: &[String],
    group_favorites: bool,
) -> Vec<T> {
    let favorites: HashSet<&str> = favorite_models.iter().map(String::as_str).collect();
    let by_slug = rank(model_order.iter().map(String::as_str));
    let original: Vec<String> = models.iter().map(|model| slug(model).to_owned()).collect();
    let original = rank(original.iter().map(String::as_str));
    let key = |model: &T| {
        let slug = slug(model);
        (
            group_favorites && !favorites.contains(slug),
            by_slug.get(slug).copied().unwrap_or(usize::MAX),
            original.get(slug).copied().unwrap_or(usize::MAX),
        )
    };
    let mut keyed: Vec<_> = models
        .into_iter()
        .map(|model| (key(&model), model))
        .collect();
    keyed.sort_by_key(|(key, _)| *key);
    keyed.into_iter().map(|(_, model)| model).collect()
}

/// Models across instances: favorites first when grouped, then
/// `instance_order`, then the given order.
pub fn sort_provider_model_items<T>(
    items: Vec<T>,
    key_of: impl Fn(&T) -> (&str, &str),
    favorite_model_keys: &HashSet<String>,
    group_favorites: bool,
    instance_order: &[String],
) -> Vec<T> {
    let instances = rank(instance_order.iter().map(String::as_str));
    let keys: Vec<String> = items
        .iter()
        .map(|item| {
            let (instance, slug) = key_of(item);
            provider_model_key(instance, slug)
        })
        .collect();
    let original = rank(keys.iter().map(String::as_str));
    let mut keyed: Vec<_> = items
        .into_iter()
        .zip(&keys)
        .map(|(item, model_key)| {
            let (instance, _) = key_of(&item);
            let key = (
                group_favorites && !favorite_model_keys.contains(model_key),
                instances.get(instance).copied().unwrap_or(usize::MAX),
                original
                    .get(model_key.as_str())
                    .copied()
                    .unwrap_or(usize::MAX),
            );
            (key, item)
        })
        .collect();
    keyed.sort_by_key(|(key, _)| *key);
    keyed.into_iter().map(|(_, item)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }

    #[test]
    fn groups_favorites_first_while_preserving_provider_model_order_inside_each_group() {
        let models = strings(&["gpt-5.5", "gpt-5.4-mini", "crest-alpha", "gpt-5.3-codex"]);
        assert_eq!(
            sort_models_for_provider_instance(
                models,
                String::as_str,
                &strings(&["gpt-5.4-mini", "gpt-5.5", "crest-alpha", "gpt-5.3-codex"]),
                &strings(&["gpt-5.5", "gpt-5.4-mini", "crest-alpha"]),
                true,
            ),
            ["gpt-5.4-mini", "gpt-5.5", "crest-alpha", "gpt-5.3-codex"]
        );
    }

    #[test]
    fn sorts_the_favorites_view_by_provider_order_then_provider_model_order() {
        let items = [
            ("codex_work", "gpt-5.4-mini"),
            ("codex_work", "gpt-5.5"),
            ("codex_work", "crest-alpha"),
            ("claudeAgent", "claude-opus-4-6"),
        ]
        .to_vec();
        let favorites = [
            ("codex_work", "gpt-5.5"),
            ("claudeAgent", "claude-opus-4-6"),
            ("codex_work", "gpt-5.4-mini"),
            ("codex_work", "crest-alpha"),
        ]
        .map(|(instance, slug)| provider_model_key(instance, slug))
        .into_iter()
        .collect();
        assert_eq!(
            sort_provider_model_items(
                items,
                |item| *item,
                &favorites,
                false,
                &strings(&["codex_work", "claudeAgent"]),
            )
            .into_iter()
            .map(|(_, slug)| slug)
            .collect::<Vec<_>>(),
            ["gpt-5.4-mini", "gpt-5.5", "crest-alpha", "claude-opus-4-6"]
        );
    }

    #[test]
    fn toggling_a_favorite_adds_then_removes_it() {
        let added = toggle_favorite(&[], "codex", "gpt-5");
        assert_eq!(
            added,
            [FavoriteModel {
                instance_id: "codex".into(),
                model: "gpt-5".into()
            }]
        );
        assert!(toggle_favorite(&added, "codex", "gpt-5").is_empty());
    }
}
