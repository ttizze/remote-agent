//! The provider instances and model catalogue the Host serves
//! (`host/provider/list`), as the model picker, the traits menu and thread
//! rows present them.
pub mod catalog_sheet;
pub mod display;
pub mod options;
pub mod ordering;
pub mod picker;
pub mod search;
pub mod staging;
pub mod switching;
pub mod traits;

use crate::{models::ProviderInstance as HostInstance, state::Snapshot};
use agent_domain::Driver;
use options::OptionDescriptor;

/// Whether an instance's catalogue can fill a picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ProviderStatus {
    /// The catalogue has not arrived yet.
    Loading,
    Ready,
    /// Usable with a caveat the message explains.
    Warning,
    Error,
    Disabled,
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
    pub enabled: bool,
    pub installed: bool,
    /// False when this Host cannot run the instance at all.
    pub available: bool,
    pub version: Option<String>,
    /// The composer offers the plan mode toggle.
    pub show_interaction_mode_toggle: bool,
    /// The provider reports its context window, so the meter shows.
    pub reports_context_window: bool,
    /// The permission modes the instance offers; empty offers all of them.
    pub supported_runtime_modes: Vec<agent_domain::RuntimeMode>,
}
impl ProviderInstance {
    /// Can contribute models to an interactive picker.
    pub fn picker_ready(&self) -> bool {
        self.enabled
            && self.installed
            && self.available
            && self.status == ProviderStatus::Ready
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
    pub aliases: Vec<String>,
    /// `new` for a recently added model.
    pub badge: Option<String>,
    pub is_default: bool,
    /// Listed under the picker's collapsed "Legacy models".
    pub is_legacy: bool,
    pub descriptors: Vec<OptionDescriptor>,
}

/// Every instance in display order and every model, grouped by instance in
/// the Host's order.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
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

/// A Host model as the picker lists it.
pub fn catalog_model(instance_id: &str, model: &crate::models::Model) -> CatalogModel {
    CatalogModel {
        instance_id: instance_id.into(),
        slug: model.slug.clone(),
        name: model.name.clone(),
        aliases: model.aliases.clone(),
        badge: model.badge.clone(),
        is_default: model.is_default,
        is_legacy: model.is_legacy,
        descriptors: model
            .option_descriptors
            .iter()
            .map(OptionDescriptor::from)
            .collect(),
    }
}

fn entry(instance: &HostInstance) -> display::ProviderEntry {
    use crate::models::ProviderStatus as Host;
    display::ProviderEntry {
        display_name: Some(instance.display_name.clone()),
        accent_color: instance.accent_color.clone(),
        status: match instance.status {
            Host::Ready => ProviderStatus::Ready,
            Host::Warning => ProviderStatus::Warning,
            Host::Error => ProviderStatus::Error,
            Host::Disabled => ProviderStatus::Disabled,
        },
        message: instance
            .unavailable_reason
            .clone()
            .or_else(|| instance.message.clone()),
        enabled: instance.enabled,
        installed: instance.installed,
        available: instance.unavailable_reason.is_none(),
        version: instance.version.clone(),
        show_interaction_mode_toggle: instance.show_interaction_mode_toggle,
        reports_context_window: instance.reports_context_window,
        supported_runtime_modes: instance.supported_runtime_modes.clone(),
        ..display::ProviderEntry::new(&instance.instance, instance.driver)
    }
}

/// The Host's instances and models. Until the Host lists them, each driver's
/// default instance is loading.
pub fn catalog(snapshot: &Snapshot) -> ModelCatalog {
    let Some(providers) = &snapshot.providers else {
        let entries: Vec<_> = DRIVERS
            .into_iter()
            .map(|driver| display::ProviderEntry {
                status: ProviderStatus::Loading,
                ..display::ProviderEntry::new(default_instance_id(driver), driver)
            })
            .collect();
        return ModelCatalog {
            instances: display::provider_instances(&entries),
            models: vec![],
        };
    };
    let entries: Vec<_> = providers.iter().map(entry).collect();
    ModelCatalog {
        instances: display::provider_instances(&entries),
        models: providers
            .iter()
            .flat_map(|instance| {
                instance
                    .models
                    .iter()
                    .map(|model| catalog_model(&instance.instance, model))
            })
            .collect(),
    }
}

/// The model a new draft starts with: the first ready instance (else the first
/// selectable one that has not failed) and its default model, else its first.
pub fn default_model(providers: &[HostInstance]) -> Option<(&HostInstance, &crate::models::Model)> {
    use crate::models::ProviderStatus as Host;
    let candidates = || {
        providers.iter().filter(|instance| {
            instance.enabled
                && instance.installed
                && instance.unavailable_reason.is_none()
                && !instance.models.is_empty()
        })
    };
    let instance = candidates()
        .find(|instance| instance.status == Host::Ready)
        .or_else(|| candidates().find(|instance| instance.status != Host::Error))?;
    let model = instance
        .models
        .iter()
        .find(|model| model.is_default)
        .or_else(|| instance.models.first())?;
    Some((instance, model))
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use agent_domain::{OptionChoice, OptionDescriptor as Descriptor, SelectOption};

    pub fn host_model(slug: &str, name: &str) -> crate::models::Model {
        crate::models::Model {
            slug: slug.into(),
            name: name.into(),
            aliases: vec![],
            badge: None,
            is_default: false,
            is_legacy: false,
            option_descriptors: vec![Descriptor::Select(SelectOption {
                id: "reasoningEffort".into(),
                label: "Reasoning".into(),
                description: None,
                options: ["medium", "high"]
                    .map(|effort| OptionChoice {
                        id: effort.into(),
                        label: effort.into(),
                        description: None,
                        is_default: effort == "high",
                    })
                    .to_vec(),
                current_value: Some("high".into()),
                prompt_injected_values: vec![],
            })],
        }
    }

    pub fn host_instance(
        instance: &str,
        driver: Driver,
        models: Vec<crate::models::Model>,
    ) -> HostInstance {
        HostInstance {
            instance: instance.into(),
            driver,
            display_name: driver_display_name(driver).into(),
            accent_color: None,
            enabled: true,
            installed: true,
            version: None,
            version_advisory: None,
            status: crate::models::ProviderStatus::Ready,
            message: None,
            unavailable_reason: None,
            show_interaction_mode_toggle: true,
            reports_context_window: true,
            supported_runtime_modes: vec![],
            models,
        }
    }

    pub fn instance(instance_id: &str, driver: Driver) -> ProviderInstance {
        display::provider_instances(&[display::ProviderEntry::new(instance_id, driver)]).remove(0)
    }

    pub fn model(instance_id: &str, slug: &str, name: &str) -> CatalogModel {
        CatalogModel {
            instance_id: instance_id.into(),
            slug: slug.into(),
            name: name.into(),
            aliases: vec![],
            badge: None,
            is_default: false,
            is_legacy: false,
            descriptors: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn the_host_instances_carry_their_display_metadata_and_descriptors() {
        let mut snapshot = Snapshot::default();
        assert!(
            catalog(&snapshot)
                .instances
                .iter()
                .all(|instance| instance.status == ProviderStatus::Loading)
        );
        let mut claude = host_instance("claude", Driver::Claude, vec![]);
        claude.status = crate::models::ProviderStatus::Error;
        claude.message = Some("Claude Code is not signed in".into());
        let mut work = host_instance(
            "codex_work",
            Driver::Codex,
            vec![host_model("gpt-5.4", "GPT-5.4")],
        );
        work.accent_color = Some("#aa3300".into());
        work.display_name = "Work".into();
        work.supported_runtime_modes = vec![
            agent_domain::RuntimeMode::ApprovalRequired,
            agent_domain::RuntimeMode::FullAccess,
        ];
        snapshot.providers = Some(vec![
            host_instance(
                "codex",
                Driver::Codex,
                vec![host_model("gpt-5.5", "GPT-5.5")],
            ),
            work,
            claude,
        ]);
        let catalog = catalog(&snapshot);
        let codex = catalog.instance("codex").unwrap();
        assert_eq!(
            (codex.display_name.as_str(), codex.status, codex.show_badge),
            ("Codex", ProviderStatus::Ready, true)
        );
        let work = catalog.instance("codex_work").unwrap();
        assert_eq!(
            (work.display_name.as_str(), work.accent_color.as_deref()),
            ("Work", Some("#aa3300"))
        );
        assert_eq!(
            work.supported_runtime_modes,
            [
                agent_domain::RuntimeMode::ApprovalRequired,
                agent_domain::RuntimeMode::FullAccess
            ]
        );
        assert!(codex.supported_runtime_modes.is_empty());
        let claude = catalog.instance("claude").unwrap();
        assert_eq!(claude.status, ProviderStatus::Error);
        assert!(!claude.picker_ready());
        assert_eq!(
            claude.message.as_deref(),
            Some("Claude Code is not signed in")
        );
        let model = catalog.models_of("codex_work").next().unwrap();
        assert_eq!(model.slug, "gpt-5.4");
        assert_eq!(model.descriptors[0].id(), "reasoningEffort");
        assert_eq!(catalog.models_of("claude").count(), 0);
    }

    #[test]
    fn a_new_draft_starts_on_the_first_ready_instances_default_model() {
        let first = host_model("gpt-5.4", "GPT-5.4");
        let mut second = host_model("gpt-5.5", "GPT-5.5");
        second.is_default = true;
        let mut disabled = host_instance("codex", Driver::Codex, vec![second.clone()]);
        disabled.enabled = false;
        let mut failed = host_instance("claude", Driver::Claude, vec![first.clone()]);
        failed.status = crate::models::ProviderStatus::Error;
        let providers = vec![
            disabled,
            failed,
            host_instance("codex_work", Driver::Codex, vec![first, second]),
        ];
        let (instance, model) = default_model(&providers).unwrap();
        assert_eq!(
            (instance.instance.as_str(), model.slug.as_str()),
            ("codex_work", "gpt-5.5")
        );
        assert!(default_model(&[]).is_none());
    }
}
