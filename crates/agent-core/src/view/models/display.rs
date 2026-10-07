//! How a provider instance presents itself: its label, initials, accent
//! color and whether its icon carries the account badge.
use super::{ProviderInstance, ProviderStatus, default_instance_id, driver_display_name};
use crate::view::thread_summary::ThreadSummary;
use agent_domain::Driver;

/// What the Host reports about one configured instance.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderEntry {
    pub instance_id: String,
    pub driver: Driver,
    pub display_name: Option<String>,
    pub accent_color: Option<String>,
    pub status: ProviderStatus,
    pub message: Option<String>,
    pub enabled: bool,
    pub installed: bool,
    pub available: bool,
    pub version: Option<String>,
    pub show_interaction_mode_toggle: bool,
    pub reports_context_window: bool,
    pub supported_runtime_modes: Vec<agent_domain::RuntimeMode>,
}
impl ProviderEntry {
    /// A ready, enabled instance with no Host-chosen name.
    pub fn new(instance_id: &str, driver: Driver) -> Self {
        Self {
            instance_id: instance_id.into(),
            driver,
            display_name: None,
            accent_color: None,
            status: ProviderStatus::Ready,
            message: None,
            enabled: true,
            installed: true,
            available: true,
            version: None,
            show_interaction_mode_toggle: true,
            reports_context_window: true,
            supported_runtime_modes: vec![],
        }
    }
}

/// Title-cases a slug: `codex_personal` is "Codex Personal",
/// `myCustomInstance` is "My Custom Instance".
fn humanize_slug(slug: &str) -> String {
    let mut spaced = String::new();
    let mut previous: Option<char> = None;
    for character in slug.chars() {
        if previous.is_some_and(|c| c.is_ascii_lowercase()) && character.is_ascii_uppercase() {
            spaced.push(' ');
        }
        spaced.push(character);
        previous = Some(character);
    }
    let mut words = String::new();
    let mut in_run = false;
    for character in spaced.chars() {
        if matches!(character, '_' | '-') {
            if !in_run {
                words.push(' ');
            }
            in_run = true;
        } else {
            words.push(character);
            in_run = false;
        }
    }
    let mut out = String::new();
    let mut previous_word = false;
    for character in words.trim().chars() {
        let word = character.is_ascii_alphanumeric() || character == '_';
        if word && !previous_word {
            out.push(character.to_ascii_uppercase());
        } else {
            out.push(character);
        }
        previous_word = word;
    }
    out
}

/// A Host-chosen name that differs from the brand label, else a humanized id
/// for a non-default instance, else the brand label.
pub fn resolve_provider_instance_display_name(
    instance_id: &str,
    driver: Driver,
    display_name: Option<&str>,
) -> String {
    let trimmed = display_name.map(str::trim).filter(|name| !name.is_empty());
    let brand = driver_display_name(driver);
    if let Some(name) = trimmed
        && name != brand
    {
        return name.into();
    }
    if instance_id != default_instance_id(driver) {
        let humanized = humanize_slug(instance_id);
        if !humanized.is_empty() {
            return humanized;
        }
    }
    trimmed.unwrap_or(brand).into()
}

/// The first two characters of a single word, or the first character of each
/// of the first two words. Never splits an emoji.
pub fn provider_instance_initials(label: &str) -> String {
    let normalized = label.replace(['_', '-'], " ");
    let words: Vec<&str> = normalized.split_whitespace().collect();
    match words.as_slice() {
        [] => String::new(),
        [word] => word.chars().take(2).flat_map(char::to_uppercase).collect(),
        words => words
            .iter()
            .take(2)
            .filter_map(|word| word.chars().next())
            .flat_map(char::to_uppercase)
            .collect(),
    }
}

/// Only `#rrggbb` renders; anything else is unset.
pub fn normalize_provider_accent_color(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    let hex = trimmed.strip_prefix('#')?;
    (hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit())).then(|| trimmed.into())
}

/// An accent color is set, or several instances share the driver so its glyph
/// alone is ambiguous.
pub fn should_show_instance_badge(
    driver: Driver,
    accent_color: Option<&str>,
    drivers: &[Driver],
) -> bool {
    accent_color.is_some() || drivers.iter().filter(|other| **other == driver).count() > 1
}

pub fn provider_instances(entries: &[ProviderEntry]) -> Vec<ProviderInstance> {
    let drivers: Vec<Driver> = entries.iter().map(|entry| entry.driver).collect();
    entries
        .iter()
        .map(|entry| {
            let display_name = resolve_provider_instance_display_name(
                &entry.instance_id,
                entry.driver,
                entry.display_name.as_deref(),
            );
            let accent_color = normalize_provider_accent_color(entry.accent_color.as_deref());
            ProviderInstance {
                instance_id: entry.instance_id.clone(),
                driver: entry.driver,
                initials: provider_instance_initials(&display_name),
                show_badge: should_show_instance_badge(
                    entry.driver,
                    accent_color.as_deref(),
                    &drivers,
                ),
                display_name,
                accent_color,
                status: entry.status,
                message: entry.message.clone(),
                enabled: entry.enabled,
                installed: entry.installed,
                available: entry.available,
                version: entry.version.clone(),
                show_interaction_mode_toggle: entry.show_interaction_mode_toggle,
                reports_context_window: entry.reports_context_window,
                supported_runtime_modes: entry.supported_runtime_modes.clone(),
            }
        })
        .collect()
}

/// The instance a thread row draws: the runtime's owner after a handoff,
/// otherwise the thread's selection.
pub fn resolve_thread_provider_instance<'a>(
    instances: &'a [ProviderInstance],
    thread: &ThreadSummary,
) -> Option<&'a ProviderInstance> {
    let instance_id = thread
        .runtime
        .as_ref()
        .map_or(&thread.instance, |runtime| &runtime.provider_instance);
    instances
        .iter()
        .find(|instance| &instance.instance_id == instance_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::thread_summary::{
        RuntimeStatus,
        fixtures::{runtime, summary},
    };

    fn entry(instance_id: &str, driver: Driver, display_name: Option<&str>) -> ProviderEntry {
        ProviderEntry {
            display_name: display_name.map(Into::into),
            ..ProviderEntry::new(instance_id, driver)
        }
    }

    #[test]
    fn keeps_a_snapshot_name_that_differs_from_the_brand_label() {
        assert_eq!(
            resolve_provider_instance_display_name("codex", Driver::Codex, Some("Work")),
            "Work"
        );
    }

    #[test]
    fn humanizes_a_custom_instance_id_when_the_snapshot_only_carries_the_brand_label() {
        assert_eq!(
            resolve_provider_instance_display_name("codex_personal", Driver::Codex, Some("Codex")),
            "Codex Personal"
        );
        assert_eq!(
            resolve_provider_instance_display_name("myCustomInstance", Driver::Codex, None),
            "My Custom Instance"
        );
    }

    #[test]
    fn uses_the_brand_label_for_the_default_instance() {
        assert_eq!(
            resolve_provider_instance_display_name("codex", Driver::Codex, None),
            "Codex"
        );
    }

    #[test]
    fn initials_take_two_characters_of_one_word_or_the_first_of_two_words() {
        assert_eq!(provider_instance_initials("Codex"), "CO");
        assert_eq!(provider_instance_initials("Codex Personal"), "CP");
        assert_eq!(
            provider_instance_initials("Codex Personal Backup Account"),
            "CP"
        );
        assert_eq!(provider_instance_initials(""), "");
    }

    #[test]
    fn initials_keep_an_emoji_whole() {
        assert_eq!(provider_instance_initials("😀 Work"), "😀W");
        assert_eq!(provider_instance_initials("😀"), "😀");
    }

    #[test]
    fn accent_colors_must_be_six_digit_hex() {
        assert_eq!(
            normalize_provider_accent_color(Some("#ff8800")).as_deref(),
            Some("#ff8800")
        );
        assert_eq!(
            normalize_provider_accent_color(Some("#FF8800")).as_deref(),
            Some("#FF8800")
        );
        assert_eq!(normalize_provider_accent_color(Some("blue")), None);
        assert_eq!(normalize_provider_accent_color(Some("#fff")), None);
        assert_eq!(normalize_provider_accent_color(None), None);
        assert_eq!(normalize_provider_accent_color(Some("   ")), None);
    }

    #[test]
    fn shows_the_badge_for_an_accent_color() {
        assert!(should_show_instance_badge(
            Driver::Codex,
            Some("#ff8800"),
            &[Driver::Codex]
        ));
    }

    #[test]
    fn shows_the_badge_when_two_entries_share_a_driver_even_without_an_accent() {
        assert!(should_show_instance_badge(
            Driver::Codex,
            None,
            &[Driver::Codex, Driver::Codex]
        ));
    }

    #[test]
    fn hides_the_badge_for_a_single_instance_of_a_driver_with_no_accent() {
        assert!(!should_show_instance_badge(
            Driver::Codex,
            None,
            &[Driver::Codex, Driver::Claude]
        ));
    }

    #[test]
    fn labels_a_custom_instance_by_its_id_so_its_initials_differ_from_the_default() {
        let instances = provider_instances(&[
            entry("codex", Driver::Codex, Some("Codex")),
            entry("codex_personal", Driver::Codex, Some("Codex")),
        ]);
        let mut thread = summary("thread-1");
        thread.instance = "codex".into();
        assert_eq!(
            resolve_thread_provider_instance(&instances, &thread)
                .unwrap()
                .display_name,
            "Codex"
        );
        thread.instance = "codex_personal".into();
        let resolved = resolve_thread_provider_instance(&instances, &thread).unwrap();
        assert_eq!(resolved.display_name, "Codex Personal");
        assert_eq!(resolved.initials, "CP");
        assert!(resolved.show_badge);
    }

    #[test]
    fn uses_the_current_runtime_owner_after_a_provider_handoff() {
        let instances = provider_instances(&[
            entry("claude", Driver::Claude, None),
            entry("codex", Driver::Codex, Some("Codex")),
            entry("codex_work", Driver::Codex, Some("Codex")),
        ]);
        let mut thread = summary("thread-1");
        thread.instance = "claude".into();
        thread.runtime = Some(crate::view::thread_summary::RuntimeSummary {
            provider_instance: "codex_work".into(),
            ..runtime(RuntimeStatus::Running)
        });
        let resolved = resolve_thread_provider_instance(&instances, &thread).unwrap();
        assert_eq!(
            (resolved.driver, resolved.display_name.as_str()),
            (Driver::Codex, "Codex Work")
        );
        assert!(resolved.show_badge);
    }

    #[test]
    fn a_thread_row_hides_the_badge_for_a_single_instance_with_no_accent_color() {
        let instances = provider_instances(&[entry("codex", Driver::Codex, None)]);
        let thread = summary("thread-1");
        assert!(
            !resolve_thread_provider_instance(&instances, &thread)
                .unwrap()
                .show_badge
        );
    }

    #[test]
    fn a_thread_row_resolves_unknown_instances_to_nothing() {
        let instances = provider_instances(&[entry("codex", Driver::Codex, None)]);
        let mut thread = summary("thread-1");
        thread.instance = "ghost".into();
        assert!(resolve_thread_provider_instance(&instances, &thread).is_none());
    }
}
