//! The provider instances and model catalogue the Host serves, as the model
//! picker, the traits menu and thread rows present them.
//!
//! The Host serves one instance per driver whose id is the driver slug, and
//! reports per-instance catalogue failures in `Snapshot::model_errors`.
pub mod display;
pub mod options;
pub mod ordering;
pub mod picker;
pub mod search;
pub mod switching;
pub mod traits;

use crate::{models::Model, provider::ProviderKind, state::Snapshot};
use agent_domain::Driver;
use options::OptionDescriptor;

/// Whether an instance's catalogue can fill a picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ProviderStatus {
    /// The catalogue has not arrived yet.
    Loading,
    Ready,
    /// The Host could not list this instance's models.
    Error,
}

/// One configured provider instance, named and badged the same way in every
/// client.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProviderInstance {
    pub instance_id: String,
    pub driver: Driver,
    pub display_name: String,
    /// Up to two initials for the account badge.
    pub initials: String,
    /// `#rrggbb`, or none.
    pub accent_color: Option<String>,
    /// The icon carries the account badge (see [`display::should_show_instance_badge`]).
    pub show_badge: bool,
    pub status: ProviderStatus,
    pub message: Option<String>,
}
impl ProviderInstance {
    /// Can contribute models to an interactive picker.
    pub fn picker_ready(&self) -> bool {
        self.status == ProviderStatus::Ready
    }
}

/// A served model: its routing slug, its display name and the option
/// descriptors the traits menu offers for it.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CatalogModel {
    pub instance_id: String,
    pub slug: String,
    pub name: String,
    pub is_default: bool,
    pub descriptors: Vec<OptionDescriptor>,
}

/// Every instance in display order and every model, grouped by instance in
/// the Host's order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelCatalog {
    pub instances: Vec<ProviderInstance>,
    pub models: Vec<CatalogModel>,
}
impl ModelCatalog {
    pub fn instance(&self, instance_id: &str) -> Option<&ProviderInstance> {
        self.instances
            .iter()
            .find(|instance| instance.instance_id == instance_id)
    }
    pub fn models_of<'a>(
        &'a self,
        instance_id: &'a str,
    ) -> impl Iterator<Item = &'a CatalogModel> + Clone {
        self.models
            .iter()
            .filter(move |model| model.instance_id == instance_id)
    }
}

pub const DRIVERS: [Driver; 2] = [Driver::Codex, Driver::Claude];

/// The id of a driver's default instance, which is the driver slug.
pub fn default_instance_id(driver: Driver) -> &'static str {
    match driver {
        Driver::Codex => "codex",
        Driver::Claude => "claude",
    }
}

/// The driver's brand label.
pub fn driver_display_name(driver: Driver) -> &'static str {
    match driver {
        Driver::Codex => "Codex",
        Driver::Claude => "Claude",
    }
}

fn driver_of(provider: ProviderKind) -> Driver {
    match provider {
        ProviderKind::Codex => Driver::Codex,
        ProviderKind::Claude => Driver::Claude,
    }
}

/// A Host model as the picker lists it.
pub fn catalog_model(model: &Model) -> CatalogModel {
    let driver = driver_of(model.model.provider);
    CatalogModel {
        instance_id: default_instance_id(driver).into(),
        slug: model.id.clone(),
        name: match driver {
            Driver::Codex => options::format_codex_model_name(&model.display_name),
            Driver::Claude => model.display_name.clone(),
        },
        is_default: model.is_default == Some(true),
        descriptors: options::model_descriptors(model),
    }
}

/// The Host's instances and models. An instance is loading until the
/// catalogue or its error arrives.
pub fn catalog(snapshot: &Snapshot) -> ModelCatalog {
    let loaded = !snapshot.models.is_empty() || !snapshot.model_errors.is_empty();
    let entries: Vec<_> = DRIVERS
        .into_iter()
        .map(|driver| {
            let instance_id = default_instance_id(driver);
            let error = snapshot.model_errors.get(instance_id);
            display::ProviderEntry {
                instance_id: instance_id.into(),
                driver,
                display_name: None,
                accent_color: None,
                status: match (loaded, error) {
                    (false, _) => ProviderStatus::Loading,
                    (true, Some(_)) => ProviderStatus::Error,
                    (true, None) => ProviderStatus::Ready,
                },
                message: error.cloned(),
            }
        })
        .collect();
    ModelCatalog {
        instances: display::provider_instances(&entries),
        models: snapshot.models.iter().map(catalog_model).collect(),
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use crate::models::{ModelRef, ReasoningEffort, ServiceTier};

    pub fn host_model(provider: ProviderKind, id: &str, name: &str) -> Model {
        Model {
            id: id.into(),
            model: ModelRef {
                provider,
                id: id.into(),
            },
            display_name: name.into(),
            default_reasoning_effort: "high".into(),
            supported_reasoning_efforts: ["medium", "high"]
                .map(|effort| ReasoningEffort {
                    reasoning_effort: effort.into(),
                })
                .to_vec(),
            service_tiers: Some(vec![ServiceTier {
                id: "priority".into(),
                name: Some("Fast".into()),
            }]),
            default_service_tier: None,
            is_default: None,
        }
    }

    pub fn instance(instance_id: &str, driver: Driver) -> ProviderInstance {
        display::provider_instances(&[display::ProviderEntry {
            instance_id: instance_id.into(),
            driver,
            display_name: None,
            accent_color: None,
            status: ProviderStatus::Ready,
            message: None,
        }])
        .remove(0)
    }

    pub fn model(instance_id: &str, slug: &str, name: &str) -> CatalogModel {
        CatalogModel {
            instance_id: instance_id.into(),
            slug: slug.into(),
            name: name.into(),
            is_default: false,
            descriptors: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn the_host_catalogue_lists_both_instances_and_formats_codex_names() {
        let mut snapshot = Snapshot::default();
        assert!(
            catalog(&snapshot)
                .instances
                .iter()
                .all(|instance| instance.status == ProviderStatus::Loading)
        );
        snapshot.models = vec![host_model(ProviderKind::Codex, "gpt-5.4", "gpt-5.4-codex")];
        snapshot
            .model_errors
            .insert("claude".into(), "Claude Code is not signed in".into());
        let catalog = catalog(&snapshot);
        let codex = catalog.instance("codex").unwrap();
        assert_eq!(
            (codex.display_name.as_str(), codex.status, codex.show_badge),
            ("Codex", ProviderStatus::Ready, false)
        );
        let claude = catalog.instance("claude").unwrap();
        assert_eq!(claude.status, ProviderStatus::Error);
        assert_eq!(
            claude.message.as_deref(),
            Some("Claude Code is not signed in")
        );
        assert_eq!(catalog.models[0].name, "GPT-5.4-Codex");
        assert_eq!(catalog.models[0].instance_id, "codex");
        assert_eq!(catalog.models_of("claude").count(), 0);
    }
}
