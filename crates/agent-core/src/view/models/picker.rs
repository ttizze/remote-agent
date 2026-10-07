//! The model picker: its trigger, the provider rail, and the rows a search
//! or rail selection shows.
use super::{
    CatalogModel, ModelCatalog, ProviderInstance, catalog, default_instance_id,
    ordering::{FavoriteModel, provider_model_key, sort_provider_model_items},
    search::{SearchableModel, build_model_picker_search_text, score_model_picker_search},
    switching::{
        HandoffFacts, started_thread_model_change_block, thread_allows_provider_switch,
        thread_supports_provider_handoff,
    },
};
use crate::{
    js_text::{utf16_len, utf16_units},
    state::{Draft, Snapshot},
    view::thread_summary::ThreadSummary,
};
use agent_domain::Driver;
use std::collections::HashSet;

const MODEL_KEY_PREFIX: &str = "model:";
const LEGACY_SECTION_KEY_PREFIX: &str = "legacy-models:";

/// A row key that stays distinct for any instance id and slug.
pub fn model_picker_model_key(instance_id: &str, slug: &str) -> String {
    format!(
        "{MODEL_KEY_PREFIX}{}:{instance_id}{slug}",
        utf16_len(instance_id)
    )
}

/// The instance id and slug of a row key.
pub fn parse_model_picker_model_key(key: &str) -> Option<(String, String)> {
    let encoded = key.strip_prefix(MODEL_KEY_PREFIX)?;
    let (length, value) = encoded.split_once(':')?;
    if length.is_empty() || !length.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let length: usize = length.parse().ok()?;
    let units = utf16_units(value);
    if length > units.len() {
        return None;
    }
    Some((
        String::from_utf16(&units[..length]).ok()?,
        String::from_utf16(&units[length..]).ok()?,
    ))
}

pub fn model_picker_legacy_section_key(instance_id: &str) -> String {
    format!("{LEGACY_SECTION_KEY_PREFIX}{instance_id}")
}

pub fn parse_model_picker_legacy_section_key(key: &str) -> Option<&str> {
    key.strip_prefix(LEGACY_SECTION_KEY_PREFIX)
}

/// A rail item: favorites across instances, or one instance's models.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum PickerRail {
    Favorites,
    Instance { instance_id: String },
}
impl PickerRail {
    fn instance(instance_id: &str) -> Self {
        Self::Instance {
            instance_id: instance_id.into(),
        }
    }
}

/// The rail item a provider shortcut moves to: favorites, then each
/// selectable instance, wrapping around.
pub fn adjacent_model_picker_provider(
    entries: &[ProviderInstance],
    selected: &PickerRail,
    forward: bool,
    disabled_instance_ids: &[String],
    selectable_unavailable_instance_ids: &[String],
) -> PickerRail {
    let providers: Vec<PickerRail> = std::iter::once(PickerRail::Favorites)
        .chain(
            entries
                .iter()
                .filter(|entry| {
                    !disabled_instance_ids.contains(&entry.instance_id)
                        && (entry.picker_ready()
                            || selectable_unavailable_instance_ids.contains(&entry.instance_id))
                })
                .map(|entry| PickerRail::instance(&entry.instance_id)),
        )
        .collect();
    let count = providers.len();
    let index = match providers.iter().position(|rail| rail == selected) {
        None if forward => 0,
        None => count - 1,
        Some(index) if forward => (index + 1) % count,
        Some(index) => (index + count - 1) % count,
    };
    providers[index].clone()
}

/// What the picker's trigger shows.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ModelPickerTrigger {
    pub instance_id: String,
    /// The instance's icon, initials and badge; none for an unknown instance.
    pub instance: Option<ProviderInstance>,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PickerRailItem {
    pub rail: PickerRail,
    /// Absent for favorites.
    pub instance: Option<ProviderInstance>,
    pub label: String,
    pub tooltip: String,
    pub selected: bool,
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ModelPickerRow {
    pub key: String,
    pub instance_id: String,
    pub driver: Driver,
    pub slug: String,
    pub name: String,
    pub provider_name: String,
    pub favorite: bool,
    pub selected: bool,
    /// Why choosing this model would fail; the row is disabled.
    pub disabled_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ModelPickerView {
    pub trigger: ModelPickerTrigger,
    pub selected_rail: PickerRail,
    /// Empty while searching.
    pub rail: Vec<PickerRailItem>,
    pub rows: Vec<ModelPickerRow>,
    /// "No models found" when nothing matches.
    pub empty_label: Option<String>,
    /// Only the thread's own driver can be chosen.
    pub locked_driver: Option<Driver>,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ModelPickerOptions {
    pub query: String,
    /// The rail item the user chose; `None` opens on the initial one.
    pub rail: Option<PickerRail>,
    pub favorites: Vec<FavoriteModel>,
}

/// The selected model, or the instance's first model when the selection is
/// not in its catalogue.
pub fn trigger(catalog: &ModelCatalog, instance_id: &str, model: &str) -> ModelPickerTrigger {
    let selected = catalog
        .models_of(instance_id)
        .find(|candidate| candidate.slug == model)
        .or_else(|| catalog.models_of(instance_id).next());
    ModelPickerTrigger {
        instance_id: instance_id.into(),
        instance: catalog.instance(instance_id).cloned(),
        label: selected.map_or_else(
            || {
                if model.is_empty() {
                    "Choose model".into()
                } else {
                    model.into()
                }
            },
            |selected| selected.name.clone(),
        ),
    }
}

fn describe_unavailable(instance: &ProviderInstance) -> String {
    let label = &instance.display_name;
    if instance.picker_ready() {
        return label.clone();
    }
    let kind = match instance.status {
        super::ProviderStatus::Error => "Unavailable",
        _ => "Not ready",
    };
    match instance.message.as_deref().map(str::trim) {
        Some(message) if !message.is_empty() => format!("{label} — {kind}. {message}"),
        _ => format!("{label} — {kind}."),
    }
}

/// The picker over a catalogue. `disabled_reason(instance, slug)` explains a
/// model the current thread cannot take.
pub fn build_model_picker(
    catalog: &ModelCatalog,
    active_instance: &str,
    model: &str,
    locked_driver: Option<Driver>,
    disabled_reason: &dyn Fn(&str, &str) -> Option<String>,
    options: &ModelPickerOptions,
) -> ModelPickerView {
    let active_key = catalog
        .models_of(active_instance)
        .find(|candidate| candidate.slug == model)
        .map_or(model, |candidate| candidate.slug.as_str());
    let active_key =
        (!active_key.is_empty()).then(|| model_picker_model_key(active_instance, active_key));
    let selected_rail = options.rail.clone().unwrap_or_else(|| {
        if locked_driver.is_none() && !options.favorites.is_empty() {
            PickerRail::Favorites
        } else {
            PickerRail::instance(active_instance)
        }
    });
    let favorites: HashSet<String> = options
        .favorites
        .iter()
        .map(|favorite| provider_model_key(&favorite.instance_id, &favorite.model))
        .collect();
    let matches_lock =
        |instance: &ProviderInstance| locked_driver.is_none_or(|driver| instance.driver == driver);
    let flat: Vec<(&ProviderInstance, &CatalogModel)> = catalog
        .instances
        .iter()
        .filter(|instance| instance.picker_ready())
        .flat_map(|instance| {
            catalog
                .models_of(&instance.instance_id)
                .map(move |model| (instance, model))
        })
        .collect();
    let is_favorite = |(_, model): &(&ProviderInstance, &CatalogModel)| {
        favorites.contains(&provider_model_key(&model.instance_id, &model.slug))
    };
    let searching = !options.query.trim().is_empty();
    let visible: Vec<(&ProviderInstance, &CatalogModel)> = if searching {
        let driver_kind = |instance: &ProviderInstance| default_instance_id(instance.driver);
        let mut ranked: Vec<_> = flat
            .iter()
            .filter(|(instance, _)| matches_lock(instance))
            .filter_map(|item| {
                let (instance, model) = item;
                let searchable = SearchableModel {
                    driver_kind: driver_kind(instance),
                    provider_display_name: &instance.display_name,
                    name: &model.name,
                    is_favorite: is_favorite(item),
                    ..Default::default()
                };
                let score = score_model_picker_search(&searchable, &options.query)?;
                Some((
                    score,
                    !is_favorite(item),
                    build_model_picker_search_text(&searchable),
                    *item,
                ))
            })
            .collect();
        ranked.sort_by(|left, right| (left.0, left.1, &left.2).cmp(&(right.0, right.1, &right.2)));
        ranked.into_iter().map(|(_, _, _, item)| item).collect()
    } else {
        let in_rail: Vec<_> = flat
            .iter()
            .copied()
            .filter(|(instance, _)| matches_lock(instance))
            .filter(|item| match &selected_rail {
                PickerRail::Favorites => is_favorite(item),
                PickerRail::Instance { instance_id } => &item.1.instance_id == instance_id,
            })
            .collect();
        let favorites_rail = selected_rail == PickerRail::Favorites;
        let order: Vec<String> = if favorites_rail {
            catalog
                .instances
                .iter()
                .map(|instance| instance.instance_id.clone())
                .collect()
        } else {
            vec![]
        };
        sort_provider_model_items(
            in_rail,
            |(_, model)| (model.instance_id.as_str(), model.slug.as_str()),
            &favorites,
            !favorites_rail,
            &order,
        )
    };
    let rows: Vec<ModelPickerRow> = visible
        .into_iter()
        .map(|item| {
            let (instance, model) = item;
            let key = model_picker_model_key(&model.instance_id, &model.slug);
            ModelPickerRow {
                selected: Some(&key) == active_key.as_ref(),
                key,
                instance_id: model.instance_id.clone(),
                driver: instance.driver,
                slug: model.slug.clone(),
                name: model.name.clone(),
                provider_name: instance.display_name.clone(),
                favorite: is_favorite(&item),
                disabled_reason: disabled_reason(&model.instance_id, &model.slug),
            }
        })
        .collect();
    let rail = if searching {
        vec![]
    } else {
        let (available, locked): (Vec<_>, Vec<_>) = catalog
            .instances
            .iter()
            .partition(|instance| matches_lock(instance));
        std::iter::once(PickerRailItem {
            selected: selected_rail == PickerRail::Favorites,
            rail: PickerRail::Favorites,
            instance: None,
            label: "Favorites".into(),
            tooltip: "Favorites".into(),
            disabled: false,
        })
        .chain(available.into_iter().chain(locked).map(|instance| {
            let rail = PickerRail::instance(&instance.instance_id);
            let context_disabled = !matches_lock(instance);
            PickerRailItem {
                selected: selected_rail == rail,
                rail,
                instance: Some(instance.clone()),
                label: instance.display_name.clone(),
                tooltip: if !instance.picker_ready() {
                    describe_unavailable(instance)
                } else if context_disabled {
                    format!(
                        "{} is unavailable in this thread. Start a new thread to switch providers.",
                        instance.display_name
                    )
                } else {
                    instance.display_name.clone()
                },
                disabled: !instance.picker_ready() || context_disabled,
            }
        }))
        .collect()
    };
    ModelPickerView {
        trigger: trigger(catalog, active_instance, model),
        selected_rail,
        empty_label: rows.is_empty().then(|| "No models found".into()),
        rail,
        rows,
        locked_driver,
    }
}

/// The selected thread's shell row and loaded handoff facts.
fn selected_thread(snapshot: &Snapshot) -> Option<(ThreadSummary, Option<HandoffFacts>)> {
    let id = snapshot.selected_thread.as_ref()?;
    let shell = snapshot.shell_view();
    let row = shell
        .as_ref()
        .and_then(|shell| shell.threads.iter().find(|row| &row.id == id))
        .or_else(|| snapshot.thread_row(id))?;
    Some((
        ThreadSummary::from_shell(row),
        snapshot.thread_state(id).and_then(HandoffFacts::of),
    ))
}

/// The picker for a draft: a started thread locks to its driver unless it can
/// hand off, and disables models it cannot take.
pub fn model_picker(
    snapshot: &Snapshot,
    draft: &Draft,
    options: &ModelPickerOptions,
) -> ModelPickerView {
    let catalog = catalog(snapshot);
    let thread = selected_thread(snapshot);
    let locked_driver = thread.as_ref().and_then(|(summary, facts)| {
        (!thread_allows_provider_switch(*facts, Some(summary))).then(|| {
            let current = summary
                .runtime
                .as_ref()
                .map_or(&summary.instance, |runtime| &runtime.provider_instance);
            catalog
                .instance(current)
                .map_or(draft.driver, |instance| instance.driver)
        })
    });
    let reason = |instance: &str, slug: &str| {
        let (summary, facts) = thread.as_ref()?;
        let current = summary
            .runtime
            .as_ref()
            .map_or(&summary.instance, |runtime| &runtime.provider_instance);
        started_thread_model_change_block(
            summary.runtime.is_some(),
            thread_supports_provider_handoff(*facts),
            current,
            &summary.model,
            instance,
            slug,
        )
        .map(|block| {
            format!(
                "{} Start a new thread to use this model.",
                block.description
            )
        })
    };
    build_model_picker(
        &catalog,
        &draft.instance_id,
        &draft.model,
        locked_driver,
        &reason,
        options,
    )
}

#[cfg(test)]
mod tests {
    use super::super::{
        ProviderStatus,
        fixtures::{instance, model},
    };
    use super::*;

    fn catalog_of(instances: Vec<ProviderInstance>, models: Vec<CatalogModel>) -> ModelCatalog {
        ModelCatalog { instances, models }
    }

    #[test]
    fn keeps_model_and_legacy_section_keys_distinct_for_colliding_instance_names() {
        let model_key = model_picker_model_key("legacy-models", "codex");
        let section_key = model_picker_legacy_section_key("codex");
        assert_ne!(model_key, section_key);
        assert_eq!(parse_model_picker_legacy_section_key(&model_key), None);
        assert_eq!(
            parse_model_picker_model_key(&model_key),
            Some(("legacy-models".into(), "codex".into()))
        );
        assert_eq!(
            parse_model_picker_model_key(&model_picker_model_key("custom", "model:😀")),
            Some(("custom".into(), "model:😀".into()))
        );
        assert_eq!(parse_model_picker_model_key("model:x:abc"), None);
        assert_eq!(parse_model_picker_model_key("model:9:abc"), None);
    }

    fn rail_entries() -> (ProviderInstance, ProviderInstance, ProviderInstance) {
        let unavailable = ProviderInstance {
            status: ProviderStatus::Error,
            ..instance("opencode_work", Driver::Codex)
        };
        (
            instance("codex_work", Driver::Codex),
            unavailable,
            instance("claude_work", Driver::Claude),
        )
    }

    #[test]
    fn wraps_through_favorites_and_ready_instances_skipping_unavailable_providers() {
        let (codex, unavailable, claude) = rail_entries();
        let entries = [codex.clone(), unavailable, claude.clone()];
        let adjacent = |selected: &PickerRail, forward| {
            adjacent_model_picker_provider(&entries, selected, forward, &[], &[])
        };
        assert_eq!(
            adjacent(&PickerRail::instance("codex_work"), true),
            PickerRail::instance("claude_work")
        );
        assert_eq!(
            adjacent(&PickerRail::Favorites, false),
            PickerRail::instance("claude_work")
        );
        assert_eq!(
            adjacent(&PickerRail::instance("claude_work"), true),
            PickerRail::Favorites
        );
    }

    #[test]
    fn keeps_thread_locks_and_the_selected_unavailable_catalog() {
        let (codex, unavailable, claude) = rail_entries();
        let entries = [codex, unavailable, claude];
        assert_eq!(
            adjacent_model_picker_provider(
                &entries,
                &PickerRail::instance("codex_work"),
                true,
                &["claude_work".into()],
                &[]
            ),
            PickerRail::Favorites
        );
        assert_eq!(
            adjacent_model_picker_provider(
                &entries,
                &PickerRail::instance("codex_work"),
                true,
                &[],
                &["opencode_work".into()]
            ),
            PickerRail::instance("opencode_work")
        );
    }

    #[test]
    fn handles_an_empty_catalog_and_a_removed_selection_in_either_direction() {
        let (codex, unavailable, claude) = rail_entries();
        let entries = [codex, unavailable, claude];
        assert_eq!(
            adjacent_model_picker_provider(
                &[],
                &PickerRail::instance("codex_work"),
                false,
                &[],
                &[]
            ),
            PickerRail::Favorites
        );
        assert_eq!(
            adjacent_model_picker_provider(
                &entries,
                &PickerRail::instance("opencode_work"),
                true,
                &[],
                &[]
            ),
            PickerRail::Favorites
        );
        assert_eq!(
            adjacent_model_picker_provider(
                &entries,
                &PickerRail::instance("opencode_work"),
                false,
                &[],
                &[]
            ),
            PickerRail::instance("claude_work")
        );
    }

    #[test]
    fn the_trigger_uses_the_first_option_label_for_a_missing_model() {
        for driver in [Driver::Codex, Driver::Claude] {
            let catalog = catalog_of(
                vec![instance("work", driver)],
                vec![model("work", "fallback-model", "Fallback model")],
            );
            assert_eq!(
                trigger(&catalog, "work", "missing-model").label,
                "Fallback model"
            );
        }
    }

    #[test]
    fn the_trigger_uses_the_first_option_when_the_active_instance_entry_is_missing() {
        let catalog = catalog_of(
            vec![],
            vec![model(
                "missing_instance",
                "fallback-model",
                "Fallback model",
            )],
        );
        let trigger = trigger(&catalog, "missing_instance", "missing-model");
        assert_eq!(trigger.label, "Fallback model");
        assert_eq!(trigger.instance, None);
    }

    #[test]
    fn the_trigger_prefers_the_matching_model_and_prompts_without_a_catalogue() {
        let catalog = catalog_of(
            vec![instance("codex", Driver::Codex)],
            vec![
                model("codex", "fallback", "Fallback model"),
                model("codex", "selected", "Selected model"),
            ],
        );
        assert_eq!(
            trigger(&catalog, "codex", "selected").label,
            "Selected model"
        );
        let empty = catalog_of(vec![instance("codex", Driver::Codex)], vec![]);
        assert_eq!(trigger(&empty, "codex", "").label, "Choose model");
        assert_eq!(trigger(&empty, "codex", "gpt-5").label, "gpt-5");
    }

    #[test]
    fn keeps_instance_initials_visible_in_the_resting_trigger() {
        let instances = super::super::display::provider_instances(&[
            super::super::display::ProviderEntry {
                instance_id: "codex".into(),
                driver: Driver::Codex,
                display_name: None,
                accent_color: None,
                status: ProviderStatus::Ready,
                message: None,
            },
            super::super::display::ProviderEntry {
                instance_id: "codex_personal".into(),
                driver: Driver::Codex,
                display_name: None,
                accent_color: None,
                status: ProviderStatus::Ready,
                message: None,
            },
        ]);
        let catalog = catalog_of(instances, vec![]);
        let trigger = trigger(&catalog, "codex_personal", "gpt-5");
        let instance = trigger.instance.unwrap();
        assert_eq!(instance.initials, "CP");
        assert!(instance.show_badge);
    }

    fn two_provider_catalog() -> ModelCatalog {
        catalog_of(
            vec![
                instance("codex", Driver::Codex),
                instance("claude", Driver::Claude),
            ],
            vec![
                model("codex", "gpt-5.5", "GPT-5.5"),
                model("codex", "gpt-5.4-mini", "GPT-5.4-Mini"),
                model("claude", "claude-opus-5", "Claude Opus 5"),
            ],
        )
    }

    fn slugs(view: &ModelPickerView) -> Vec<&str> {
        view.rows.iter().map(|row| row.slug.as_str()).collect()
    }

    #[test]
    fn opens_on_favorites_when_there_are_any_and_groups_them_first_in_an_instance() {
        let catalog = two_provider_catalog();
        let favorites = vec![FavoriteModel {
            instance_id: "codex".into(),
            model: "gpt-5.4-mini".into(),
        }];
        let options = ModelPickerOptions {
            favorites: favorites.clone(),
            ..Default::default()
        };
        let view = build_model_picker(&catalog, "codex", "gpt-5.5", None, &|_, _| None, &options);
        assert_eq!(view.selected_rail, PickerRail::Favorites);
        assert_eq!(slugs(&view), ["gpt-5.4-mini"]);
        assert_eq!(
            view.rail
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            ["Favorites", "Codex", "Claude"]
        );
        let view = build_model_picker(
            &catalog,
            "codex",
            "gpt-5.5",
            None,
            &|_, _| None,
            &ModelPickerOptions {
                rail: Some(PickerRail::instance("codex")),
                ..options
            },
        );
        assert_eq!(slugs(&view), ["gpt-5.4-mini", "gpt-5.5"]);
        assert!(view.rows[1].selected && view.rows[0].favorite);
        let empty = build_model_picker(
            &catalog,
            "codex",
            "gpt-5.5",
            None,
            &|_, _| None,
            &Default::default(),
        );
        assert_eq!(empty.selected_rail, PickerRail::instance("codex"));
    }

    #[test]
    fn a_locked_thread_disables_other_drivers_and_keeps_them_last() {
        let catalog = catalog_of(
            vec![
                instance("claude", Driver::Claude),
                instance("codex", Driver::Codex),
            ],
            two_provider_catalog().models,
        );
        let options = ModelPickerOptions {
            favorites: vec![FavoriteModel {
                instance_id: "claude".into(),
                model: "claude-opus-5".into(),
            }],
            ..Default::default()
        };
        let view = build_model_picker(
            &catalog,
            "codex",
            "gpt-5.5",
            Some(Driver::Codex),
            &|_, _| None,
            &options,
        );
        assert_eq!(view.selected_rail, PickerRail::instance("codex"));
        let claude = view.rail.last().unwrap();
        assert!(claude.disabled);
        assert_eq!(
            claude.tooltip,
            "Claude is unavailable in this thread. Start a new thread to switch providers."
        );
        let searched = build_model_picker(
            &catalog,
            "codex",
            "gpt-5.5",
            Some(Driver::Codex),
            &|_, _| None,
            &ModelPickerOptions {
                query: "o".into(),
                ..options
            },
        );
        assert!(searched.rail.is_empty());
        assert!(searched.rows.iter().all(|row| row.driver == Driver::Codex));
    }

    #[test]
    fn a_search_spans_instances_and_reports_when_nothing_matches() {
        let catalog = two_provider_catalog();
        let search = |query: &str| {
            build_model_picker(
                &catalog,
                "codex",
                "gpt-5.5",
                None,
                &|_, _| None,
                &ModelPickerOptions {
                    query: query.into(),
                    ..Default::default()
                },
            )
        };
        assert_eq!(slugs(&search("opus")), ["claude-opus-5"]);
        assert_eq!(slugs(&search("gpt")), ["gpt-5.5", "gpt-5.4-mini"]);
        assert_eq!(
            search("zzz").empty_label.as_deref(),
            Some("No models found")
        );
    }

    #[test]
    fn unavailable_instances_list_no_models_and_explain_themselves() {
        let catalog = catalog_of(
            vec![
                instance("codex", Driver::Codex),
                ProviderInstance {
                    status: ProviderStatus::Error,
                    message: Some("Sign in to Claude Code.".into()),
                    ..instance("claude", Driver::Claude)
                },
            ],
            two_provider_catalog().models,
        );
        let view = build_model_picker(
            &catalog,
            "claude",
            "claude-opus-5",
            None,
            &|_, _| None,
            &Default::default(),
        );
        assert!(view.rows.is_empty());
        let claude = &view.rail[2];
        assert!(claude.disabled);
        assert_eq!(
            claude.tooltip,
            "Claude — Unavailable. Sign in to Claude Code."
        );
    }

    #[test]
    fn a_started_thread_without_loaded_state_locks_to_its_driver() {
        use crate::{
            provider::ProviderKind, sync::fixtures::*, view::models::fixtures::host_model,
        };
        let mut row = agent_domain::shell(&thread_state("Thread")).unwrap();
        row.latest_run = Some(agent_domain::RunId::new("run").unwrap());
        row.status = Some(agent_domain::RunStatus::Completed);
        let mut snapshot = Snapshot {
            selected_thread: Some(thread_id()),
            models: vec![
                host_model(ProviderKind::Codex, "gpt", "gpt"),
                host_model(ProviderKind::Claude, "claude-opus-5", "Claude · Opus 5"),
            ],
            ..Snapshot::default()
        };
        let mut shell = crate::sync::ShellCache::default();
        shell.snapshot = Some(agent_protocol::conversation::ShellSnapshot {
            snapshot_sequence: 1,
            projects: vec![],
            threads: vec![row],
        });
        snapshot.shell = std::sync::Arc::new(shell);
        let draft = snapshot.current_draft();
        let view = model_picker(
            &snapshot,
            &draft,
            &ModelPickerOptions {
                rail: Some(PickerRail::instance("claude")),
                ..Default::default()
            },
        );
        assert_eq!(view.locked_driver, Some(Driver::Codex));
        assert!(view.rows.is_empty());
        assert!(view.rail.last().unwrap().disabled);
        assert_eq!(view.trigger.label, "GPT");
        let searched = model_picker(
            &snapshot,
            &draft,
            &ModelPickerOptions {
                query: "gpt".into(),
                ..Default::default()
            },
        );
        assert_eq!(searched.rows.len(), 1);
        assert!(searched.rows[0].selected);
        snapshot.selected_thread = None;
        let unlocked = model_picker(
            &snapshot,
            &draft,
            &ModelPickerOptions {
                rail: Some(PickerRail::instance("claude")),
                ..Default::default()
            },
        );
        assert_eq!(unlocked.locked_driver, None);
        assert_eq!(unlocked.rows[0].disabled_reason, None);
    }

    #[test]
    fn rows_carry_the_disabled_reason() {
        let catalog = two_provider_catalog();
        let view = build_model_picker(
            &catalog,
            "codex",
            "gpt-5.5",
            None,
            &|instance, _| (instance == "claude").then(|| "blocked".to_owned()),
            &ModelPickerOptions {
                rail: Some(PickerRail::instance("claude")),
                ..Default::default()
            },
        );
        assert_eq!(view.rows[0].disabled_reason.as_deref(), Some("blocked"));
    }
}
