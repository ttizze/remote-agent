//! The mobile thread settings sheet's model catalogue: one section per
//! provider instance, primary and selected providers open, legacy models behind
//! the "Show legacy models" switch, favorites first.
use super::{ProviderInstance, ordering::provider_model_key};
use crate::state::Snapshot;
use agent_domain::Driver;

/// Which providers the catalogue lists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum CatalogFilter {
    #[default]
    All,
    Favorites,
    Instance {
        instance_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CatalogSheetOptions {
    pub filter: CatalogFilter,
    pub show_legacy: bool,
    pub query: String,
    /// Instances whose section the user opened or closed since the sheet opened.
    pub expansion_overrides: Vec<String>,
    /// The model staged in the sheet, by `instance:slug`; Save applies it.
    pub staged_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
#[allow(
    clippy::large_enum_variant,
    reason = "exported records cannot be boxed"
)]
pub enum CatalogSheetItem {
    Provider {
        key: String,
        instance: ProviderInstance,
        collapsible: bool,
        collapsed: bool,
        model_count: u32,
    },
    Model {
        /// `instance:slug`.
        key: String,
        instance_id: String,
        driver: Driver,
        slug: String,
        label: String,
        favorite: bool,
        /// The thread's current model.
        applied: bool,
        /// Highlighted: the staged model, else the applied one.
        displayed: bool,
        is_legacy: bool,
        /// The model is no longer offered.
        unavailable: bool,
        is_first: bool,
        is_last: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CatalogSheetView {
    pub items: Vec<CatalogSheetItem>,
    /// The "Show legacy models" switch shows.
    pub has_legacy_models: bool,
    /// The filter menu's providers, in catalogue order.
    pub providers: Vec<ProviderInstance>,
}

fn primary(driver: Driver) -> bool {
    matches!(driver, Driver::Codex | Driver::Claude)
}

/// Primary and selected providers start open; a disclosure tap inverts that
/// until the sheet closes. A narrowed list keeps every section open.
pub fn provider_section_is_collapsed(
    default_expanded: bool,
    has_expansion_override: bool,
    narrowed: bool,
) -> bool {
    !narrowed && default_expanded == has_expansion_override
}

struct Entry {
    key: String,
    instance_id: String,
    slug: String,
    label: String,
    is_legacy: bool,
    unavailable: bool,
}

/// The sheet's catalogue for the draft's selection.
pub fn catalog_sheet(snapshot: &Snapshot, options: &CatalogSheetOptions) -> CatalogSheetView {
    let catalog = super::catalog(snapshot);
    let draft = snapshot.current_draft();
    let applied_key =
        (!draft.model.is_empty()).then(|| provider_model_key(&draft.instance_id, &draft.model));
    let displayed_key = options.staged_key.clone().or(applied_key.clone());
    let favorites: Vec<String> = snapshot
        .preferences
        .favorite_models
        .iter()
        .map(|favorite| provider_model_key(&favorite.instance_id, &favorite.model))
        .collect();
    let listed: Vec<&ProviderInstance> = catalog
        .instances
        .iter()
        .filter(|instance| instance.enabled && instance.installed && instance.available)
        .collect();
    let query = options.query.trim().to_lowercase();
    let narrowed = options.filter != CatalogFilter::All || !query.is_empty();
    let mut has_legacy_models = false;
    let mut items = vec![];
    for instance in &listed {
        let mut entries: Vec<Entry> = catalog
            .models_of(&instance.instance_id)
            .map(|model| Entry {
                key: provider_model_key(&instance.instance_id, &model.slug),
                instance_id: instance.instance_id.clone(),
                slug: model.slug.clone(),
                label: model.name.clone(),
                is_legacy: model.is_legacy,
                unavailable: false,
            })
            .collect();
        if draft.instance_id == instance.instance_id
            && !draft.model.is_empty()
            && !entries.iter().any(|entry| entry.slug == draft.model)
        {
            entries.push(Entry {
                key: provider_model_key(&instance.instance_id, &draft.model),
                instance_id: instance.instance_id.clone(),
                slug: draft.model.clone(),
                label: draft.model.clone(),
                is_legacy: false,
                unavailable: true,
            });
        }
        has_legacy_models |= entries.iter().any(|entry| entry.is_legacy);
        if let CatalogFilter::Instance { instance_id } = &options.filter
            && instance_id != &instance.instance_id
        {
            continue;
        }
        let favorite = |entry: &Entry| favorites.contains(&entry.key);
        let shown_legacy = options.show_legacy || options.filter == CatalogFilter::Favorites;
        let contains_applied = entries
            .iter()
            .any(|entry| Some(&entry.key) == applied_key.as_ref());
        let mut visible: Vec<Entry> = entries
            .into_iter()
            .filter(|entry| {
                shown_legacy
                    || !entry.is_legacy
                    || Some(&entry.key) == displayed_key.as_ref()
                    || favorite(entry)
            })
            .filter(|entry| options.filter != CatalogFilter::Favorites || favorite(entry))
            .filter(|entry| {
                query.is_empty()
                    || [&entry.label, &entry.slug, &instance.display_name]
                        .iter()
                        .any(|value| value.to_lowercase().contains(&query))
            })
            .collect();
        visible.sort_by_key(|entry| !favorite(entry));
        if visible.is_empty() {
            continue;
        }
        let collapsed = provider_section_is_collapsed(
            primary(instance.driver) || contains_applied,
            options.expansion_overrides.contains(&instance.instance_id),
            narrowed,
        );
        items.push(CatalogSheetItem::Provider {
            key: format!("provider:{}", instance.instance_id),
            instance: (*instance).clone(),
            collapsible: !narrowed,
            collapsed,
            model_count: visible.len() as u32,
        });
        if collapsed {
            continue;
        }
        let last = visible.len() - 1;
        items.extend(visible.into_iter().enumerate().map(|(index, entry)| {
            CatalogSheetItem::Model {
                favorite: favorites.contains(&entry.key),
                applied: Some(&entry.key) == applied_key.as_ref(),
                displayed: Some(&entry.key) == displayed_key.as_ref(),
                key: format!("model:{}", entry.key),
                instance_id: entry.instance_id,
                driver: instance.driver,
                slug: entry.slug,
                label: entry.label,
                is_legacy: entry.is_legacy,
                unavailable: entry.unavailable,
                is_first: index == 0,
                is_last: index == last,
            }
        }));
    }
    CatalogSheetView {
        items,
        has_legacy_models,
        providers: listed.into_iter().cloned().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::models::fixtures::{host_instance, host_model};

    fn snapshot() -> Snapshot {
        let mut legacy = host_model("gpt-4", "GPT-4");
        legacy.is_legacy = true;
        let mut snapshot = Snapshot::default();
        snapshot.providers = Some(vec![
            host_instance(
                "codex",
                Driver::Codex,
                vec![host_model("gpt-5.5", "GPT-5.5"), legacy],
            ),
            host_instance(
                "codex_work",
                Driver::Codex,
                vec![host_model("gpt-5.4", "GPT-5.4")],
            ),
        ]);
        snapshot.default_draft.instance_id = "codex".into();
        snapshot.default_draft.model = "gpt-5.5".into();
        snapshot
    }

    fn keys(view: &CatalogSheetView) -> Vec<String> {
        view.items
            .iter()
            .map(|item| match item {
                CatalogSheetItem::Provider { key, collapsed, .. } => format!("{key}:{collapsed}"),
                CatalogSheetItem::Model { key, .. } => key.clone(),
            })
            .collect()
    }

    // ThreadSettingsSheet useThreadSettingsCatalogItems and
    // thread-settings-sheet-state.ts providerSectionIsCollapsed.
    #[test]
    fn legacy_models_hide_behind_the_switch_and_primary_sections_start_open() {
        let mut snapshot = snapshot();
        let view = catalog_sheet(&snapshot, &CatalogSheetOptions::default());
        assert!(view.has_legacy_models);
        assert_eq!(
            keys(&view),
            [
                "provider:codex:false",
                "model:codex:gpt-5.5",
                "provider:codex_work:false",
                "model:codex_work:gpt-5.4"
            ]
        );
        let shown = catalog_sheet(
            &snapshot,
            &CatalogSheetOptions {
                show_legacy: true,
                expansion_overrides: vec!["codex_work".into()],
                ..Default::default()
            },
        );
        assert_eq!(
            keys(&shown),
            [
                "provider:codex:false",
                "model:codex:gpt-5.5",
                "model:codex:gpt-4",
                "provider:codex_work:true"
            ]
        );
        snapshot.preferences.favorite_models = vec![crate::view::models::ordering::FavoriteModel {
            instance_id: "codex_work".into(),
            model: "gpt-5.4".into(),
        }];
        let favorites = catalog_sheet(
            &snapshot,
            &CatalogSheetOptions {
                filter: CatalogFilter::Favorites,
                ..Default::default()
            },
        );
        assert_eq!(
            keys(&favorites),
            ["provider:codex_work:false", "model:codex_work:gpt-5.4"]
        );
        let searched = catalog_sheet(
            &snapshot,
            &CatalogSheetOptions {
                query: "5.4".into(),
                ..Default::default()
            },
        );
        assert_eq!(
            keys(&searched),
            ["provider:codex_work:false", "model:codex_work:gpt-5.4"]
        );
    }

    #[test]
    fn a_selected_legacy_model_stays_listed() {
        let mut snapshot = snapshot();
        snapshot.default_draft.model = "gpt-4".into();
        let view = catalog_sheet(&snapshot, &CatalogSheetOptions::default());
        assert!(keys(&view).contains(&"model:codex:gpt-4".to_owned()));
    }
}
