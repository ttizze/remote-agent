//! Device-local browser preferences and profile resolution.
//!
//! Chromium state belongs to the client that presents the Preview surface.
//! This module owns only the durable values and the pure projection consumed
//! by Preview, desktop actions, and the native settings views.

use agent_protocol::preview::{
    BrowserLinkTarget, PreviewAppearance, PreviewViewportSetting, PreviewZoom,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const BROWSER_RECORDING_FRAME_RATES: [u32; 2] = [30, 60];
pub const DEFAULT_BROWSER_RECORDING_FRAME_RATE: u32 = 30;
pub const DEFAULT_BROWSER_RECORDING_SHOW_KEY_PRESSES: bool = false;
pub const DEFAULT_BROWSER_RECORDING_SHOW_MOUSE_PRESSES: bool = false;
pub const BROWSER_PROFILE_NAME_MAX_LENGTH: usize = 48;
pub const BROWSER_PROFILE_ID_MAX_LENGTH: usize = 64;
pub const BROWSER_PROFILE_MAX_COUNT: usize = 24;
pub const DEFAULT_BROWSER_PROFILE_ID: &str = "default";
pub const INCOGNITO_BROWSER_PROFILE_ID: &str = "incognito";

/// Whether Chromium retains a profile's cookies between Host restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
#[serde(rename_all = "lowercase")]
pub enum BrowserProfileKind {
    Persistent,
    Incognito,
}

/// A user-created browser identity. Built-ins are synthesized by
/// [`resolve_browser_profiles`] and therefore never need to be persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[serde(rename_all = "camelCase")]
pub struct BrowserProfile {
    pub id: String,
    pub name: String,
    pub kind: BrowserProfileKind,
}

/// The values stored in the device state file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrowserSettings {
    /// Defaults for a newly opened Preview tab. These values are client-local
    /// because the Chromium guest is owned by the presenting client.
    pub viewport: PreviewViewportSetting,
    pub zoom: PreviewZoom,
    pub appearance: PreviewAppearance,
    pub link_target: BrowserLinkTarget,
    pub auto_show_floating_preview: bool,
    pub recording_frame_rate: u32,
    pub recording_show_key_presses: bool,
    pub recording_show_mouse_presses: bool,
    /// Only user-created profiles are durable. Built-ins are synthesized.
    pub profiles: Vec<BrowserProfile>,
    pub default_profile_id: String,
}

impl Default for BrowserSettings {
    fn default() -> Self {
        Self {
            viewport: PreviewViewportSetting::Fill,
            zoom: PreviewZoom::X100,
            appearance: PreviewAppearance::System,
            link_target: BrowserLinkTarget::System,
            auto_show_floating_preview: true,
            recording_frame_rate: DEFAULT_BROWSER_RECORDING_FRAME_RATE,
            recording_show_key_presses: DEFAULT_BROWSER_RECORDING_SHOW_KEY_PRESSES,
            recording_show_mouse_presses: DEFAULT_BROWSER_RECORDING_SHOW_MOUSE_PRESSES,
            profiles: vec![],
            default_profile_id: DEFAULT_BROWSER_PROFILE_ID.into(),
        }
    }
}

impl BrowserSettings {
    /// Validates a value before it is persisted by a device intent.
    pub fn validate(&self) -> Result<(), String> {
        validate_browser_viewport(&self.viewport)?;
        if !BROWSER_RECORDING_FRAME_RATES.contains(&self.recording_frame_rate) {
            return Err(format!(
                "browser recording frame rate must be {} or {}",
                BROWSER_RECORDING_FRAME_RATES[0], BROWSER_RECORDING_FRAME_RATES[1]
            ));
        }
        validate_browser_profile_id(&self.default_profile_id)?;
        if self.profiles.len() > BROWSER_PROFILE_MAX_COUNT {
            return Err(format!(
                "browser profiles are limited to {BROWSER_PROFILE_MAX_COUNT} custom profiles"
            ));
        }
        for profile in &self.profiles {
            validate_profile(profile)?;
        }
        Ok(())
    }

    /// The complete profile picker state and the profile used for new tabs.
    pub fn resolved(&self) -> BrowserDefaults {
        let profiles = resolve_browser_profiles(&self.profiles);
        let profile_id = profiles
            .iter()
            .find(|profile| {
                profile.id == self.default_profile_id
                    && profile.kind == BrowserProfileKind::Persistent
            })
            .map(|profile| profile.id.clone())
            .unwrap_or_else(|| DEFAULT_BROWSER_PROFILE_ID.into());
        BrowserDefaults {
            viewport: self.viewport.clone(),
            zoom: self.zoom,
            appearance: self.appearance,
            link_target: self.link_target,
            auto_show_floating_preview: self.auto_show_floating_preview,
            recording_frame_rate: self.recording_frame_rate,
            recording_show_key_presses: self.recording_show_key_presses,
            recording_show_mouse_presses: self.recording_show_mouse_presses,
            profiles,
            profile_id,
        }
    }
}

/// The resolved browser values consumed by a Preview opener.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct BrowserDefaults {
    pub viewport: PreviewViewportSetting,
    pub zoom: PreviewZoom,
    pub appearance: PreviewAppearance,
    pub link_target: BrowserLinkTarget,
    pub auto_show_floating_preview: bool,
    pub recording_frame_rate: u32,
    pub recording_show_key_presses: bool,
    pub recording_show_mouse_presses: bool,
    pub profiles: Vec<BrowserProfile>,
    pub profile_id: String,
}

/// Built-ins are always first, cannot be edited or removed, and are never
/// shadowed by hand-edited persisted data.
pub fn resolve_browser_profiles(user_profiles: &[BrowserProfile]) -> Vec<BrowserProfile> {
    let mut seen = BTreeSet::from([
        DEFAULT_BROWSER_PROFILE_ID.to_owned(),
        INCOGNITO_BROWSER_PROFILE_ID.to_owned(),
    ]);
    let mut resolved = vec![
        BrowserProfile {
            id: DEFAULT_BROWSER_PROFILE_ID.into(),
            name: "Default".into(),
            kind: BrowserProfileKind::Persistent,
        },
        BrowserProfile {
            id: INCOGNITO_BROWSER_PROFILE_ID.into(),
            name: "Incognito".into(),
            kind: BrowserProfileKind::Incognito,
        },
    ];
    for profile in user_profiles {
        if !seen.insert(profile.id.clone()) {
            continue;
        }
        resolved.push(BrowserProfile {
            id: profile.id.clone(),
            name: profile.name.clone(),
            // Persistence is keyed by the built-in id alone. A custom entry
            // claiming incognito would otherwise promise ephemeral cookies it
            // cannot provide.
            kind: BrowserProfileKind::Persistent,
        });
    }
    resolved
}

pub fn is_built_in_browser_profile_id(id: &str) -> bool {
    matches!(id, DEFAULT_BROWSER_PROFILE_ID | INCOGNITO_BROWSER_PROFILE_ID)
}

pub fn find_browser_profile<'a>(
    profiles: &'a [BrowserProfile],
    id: Option<&str>,
) -> Option<&'a BrowserProfile> {
    id.and_then(|id| profiles.iter().find(|profile| profile.id == id))
}

pub fn validate_browser_profile_id(id: &str) -> Result<(), String> {
    if id.trim() != id || id.is_empty() {
        return Err("browser profile ids must be non-empty and trimmed".into());
    }
    if id.chars().count() > BROWSER_PROFILE_ID_MAX_LENGTH {
        return Err(format!(
            "browser profile ids are limited to {BROWSER_PROFILE_ID_MAX_LENGTH} characters"
        ));
    }
    if id.chars().any(char::is_control) {
        return Err("browser profile ids cannot contain control characters".into());
    }
    Ok(())
}

fn validate_profile(profile: &BrowserProfile) -> Result<(), String> {
    validate_browser_profile_id(&profile.id)?;
    if profile.name.trim() != profile.name || profile.name.is_empty() {
        return Err("browser profile names must be non-empty and trimmed".into());
    }
    if profile.name.chars().count() > BROWSER_PROFILE_NAME_MAX_LENGTH {
        return Err(format!(
            "browser profile names are limited to {BROWSER_PROFILE_NAME_MAX_LENGTH} characters"
        ));
    }
    Ok(())
}

pub fn validate_browser_viewport(viewport: &PreviewViewportSetting) -> Result<(), String> {
    let (width, height) = match viewport {
        PreviewViewportSetting::Fill => return Ok(()),
        PreviewViewportSetting::Freeform { width, height }
        | PreviewViewportSetting::Preset { width, height, .. } => (*width, *height),
    };
    let range = agent_protocol::preview::PREVIEW_VIEWPORT_MIN_DIMENSION
        ..=agent_protocol::preview::PREVIEW_VIEWPORT_MAX_DIMENSION;
    if !range.contains(&width) || !range.contains(&height) {
        return Err(format!(
            "browser viewport sides need {} to {} pixels",
            agent_protocol::preview::PREVIEW_VIEWPORT_MIN_DIMENSION,
            agent_protocol::preview::PREVIEW_VIEWPORT_MAX_DIMENSION
        ));
    }
    if u64::from(width) * u64::from(height)
        > agent_protocol::preview::PREVIEW_VIEWPORT_MAX_AREA
    {
        return Err(format!(
            "browser viewport area must not exceed {} pixels",
            agent_protocol::preview::PREVIEW_VIEWPORT_MAX_AREA
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work() -> BrowserProfile {
        BrowserProfile {
            id: "profile-work".into(),
            name: "Work".into(),
            kind: BrowserProfileKind::Persistent,
        }
    }

    #[test]
    fn defaults_match_the_client_contract() {
        let settings = BrowserSettings::default();
        assert_eq!(settings.recording_frame_rate, 30);
        assert!(!settings.recording_show_key_presses);
        assert!(!settings.recording_show_mouse_presses);
        let resolved = settings.resolved();
        assert_eq!(resolved.profile_id, DEFAULT_BROWSER_PROFILE_ID);
        assert_eq!(
            resolved.profiles.iter().map(|profile| profile.id.as_str()).collect::<Vec<_>>(),
            vec![DEFAULT_BROWSER_PROFILE_ID, INCOGNITO_BROWSER_PROFILE_ID]
        );
    }

    #[test]
    fn built_ins_are_synthesized_and_custom_ids_are_first_wins() {
        let settings = BrowserSettings {
            profiles: vec![
                BrowserProfile {
                    id: DEFAULT_BROWSER_PROFILE_ID.into(),
                    name: "Hijacked".into(),
                    kind: BrowserProfileKind::Persistent,
                },
                BrowserProfile {
                    id: INCOGNITO_BROWSER_PROFILE_ID.into(),
                    name: "Not incognito".into(),
                    kind: BrowserProfileKind::Persistent,
                },
                work(),
                BrowserProfile {
                    id: "profile-work".into(),
                    name: "Old Work".into(),
                    kind: BrowserProfileKind::Persistent,
                },
            ],
            ..Default::default()
        };
        let resolved = settings.resolved();
        assert_eq!(resolved.profiles[0].name, "Default");
        assert_eq!(resolved.profiles[1].kind, BrowserProfileKind::Incognito);
        assert_eq!(resolved.profiles[2], work());
        assert_eq!(resolved.profiles.len(), 3);
    }

    #[test]
    fn custom_incognito_claim_is_persisted_as_a_persistent_profile() {
        let resolved = resolve_browser_profiles(&[BrowserProfile {
            id: "throwaway".into(),
            name: "Throwaway".into(),
            kind: BrowserProfileKind::Incognito,
        }]);
        assert_eq!(resolved[2].kind, BrowserProfileKind::Persistent);
    }

    #[test]
    fn missing_or_incognito_default_falls_back_to_default() {
        let mut settings = BrowserSettings {
            default_profile_id: "missing".into(),
            profiles: vec![work()],
            ..Default::default()
        };
        assert_eq!(settings.resolved().profile_id, DEFAULT_BROWSER_PROFILE_ID);
        settings.default_profile_id = INCOGNITO_BROWSER_PROFILE_ID.into();
        assert_eq!(settings.resolved().profile_id, DEFAULT_BROWSER_PROFILE_ID);
        settings.default_profile_id = "profile-work".into();
        assert_eq!(settings.resolved().profile_id, "profile-work");
    }

    #[test]
    fn validation_keeps_recording_bounds_and_profile_ids_explicit() {
        for frame_rate in [30, 60] {
            let settings = BrowserSettings {
                recording_frame_rate: frame_rate,
                ..Default::default()
            };
            assert!(settings.validate().is_ok());
        }
        for frame_rate in [24, 59, 120] {
            let settings = BrowserSettings {
                recording_frame_rate: frame_rate,
                ..Default::default()
            };
            assert!(settings.validate().is_err());
        }
        let settings = BrowserSettings {
            profiles: vec![BrowserProfile {
                id: "profile-a\0b".into(),
                name: "A".into(),
                kind: BrowserProfileKind::Persistent,
            }],
            ..Default::default()
        };
        assert!(settings.validate().is_err());
    }
}
