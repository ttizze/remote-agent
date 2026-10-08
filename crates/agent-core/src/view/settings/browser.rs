//! Device-local Preview browser defaults and recording controls.
use super::{
    SettingChoice, SettingControl, SettingId, SettingValue, SettingsRow, SettingsScope, choice,
    registry::{Context, Section},
    row, section,
};
use crate::{
    state::{Intent, Preferences},
    view::browser::{BrowserProfileKind, BrowserSettings},
};
use agent_protocol::preview::{
    BrowserLinkTarget, PREVIEW_VIEWPORT_MAX_DIMENSION, PREVIEW_VIEWPORT_MIN_DIMENSION,
    PREVIEW_VIEWPORT_PRESETS, PreviewAppearance, PreviewViewportPreset, PreviewViewportSetting,
    PreviewZoom,
};

const RESPONSIVE_VIEWPORT_ID: &str = "responsive";

fn viewport_preset_id(preset: PreviewViewportPreset) -> &'static str {
    match preset {
        PreviewViewportPreset::IphoneSe => "iphone-se",
        PreviewViewportPreset::IphoneXr => "iphone-xr",
        PreviewViewportPreset::Iphone12Pro => "iphone-12-pro",
        PreviewViewportPreset::Iphone14ProMax => "iphone-14-pro-max",
        PreviewViewportPreset::Pixel7 => "pixel-7",
        PreviewViewportPreset::SamsungGalaxyS8Plus => "samsung-galaxy-s8-plus",
        PreviewViewportPreset::SamsungGalaxyS20Ultra => "samsung-galaxy-s20-ultra",
        PreviewViewportPreset::IpadMini => "ipad-mini",
        PreviewViewportPreset::IpadAir => "ipad-air",
        PreviewViewportPreset::IpadPro => "ipad-pro",
        PreviewViewportPreset::SurfacePro7 => "surface-pro-7",
        PreviewViewportPreset::SurfaceDuo => "surface-duo",
        PreviewViewportPreset::GalaxyZFold5 => "galaxy-z-fold-5",
        PreviewViewportPreset::AsusZenbookFold => "asus-zenbook-fold",
        PreviewViewportPreset::SamsungGalaxyA5171 => "samsung-galaxy-a51-71",
        PreviewViewportPreset::NestHub => "nest-hub",
        PreviewViewportPreset::NestHubMax => "nest-hub-max",
    }
}

fn viewport_id(viewport: &PreviewViewportSetting) -> String {
    match viewport {
        PreviewViewportSetting::Fill => "fill".into(),
        PreviewViewportSetting::Freeform { .. } => RESPONSIVE_VIEWPORT_ID.into(),
        PreviewViewportSetting::Preset { preset, .. } => viewport_preset_id(*preset).into(),
    }
}

fn viewport_choices() -> Vec<SettingChoice> {
    let mut choices = vec![
        choice(
            "fill",
            "Fill panel",
            Some("Use the Preview panel's current size."),
        ),
        choice(
            RESPONSIVE_VIEWPORT_ID,
            "Responsive",
            Some("Use a custom width and height for newly opened tabs."),
        ),
    ];
    choices.extend(
        PREVIEW_VIEWPORT_PRESETS
            .iter()
            .map(|preset| choice(viewport_preset_id(preset.id), preset.label, None)),
    );
    choices
}

fn viewport_from_id(id: &str, current: &PreviewViewportSetting) -> Option<PreviewViewportSetting> {
    if id == "fill" {
        return Some(PreviewViewportSetting::Fill);
    }
    if id == RESPONSIVE_VIEWPORT_ID {
        return Some(match current {
            PreviewViewportSetting::Freeform { .. } => *current,
            _ => PreviewViewportSetting::Freeform {
                width: 1_280,
                height: 800,
            },
        });
    }
    PREVIEW_VIEWPORT_PRESETS
        .iter()
        .find(|preset| viewport_preset_id(preset.id) == id)
        .map(|preset| PreviewViewportSetting::Preset {
            preset: preset.id,
            width: preset.width,
            height: preset.height,
        })
}

fn zoom_id(zoom: PreviewZoom) -> String {
    format!("{}", (zoom.factor() * 100.0).round() as u32)
}

fn zoom_choices() -> Vec<SettingChoice> {
    PreviewZoom::LEVELS
        .iter()
        .copied()
        .map(|zoom| {
            let percent = (zoom.factor() * 100.0).round() as u32;
            choice(&zoom_id(zoom), &format!("{percent}%"), None)
        })
        .collect()
}

fn zoom_from_id(id: &str) -> Option<PreviewZoom> {
    let percent = id.parse::<u32>().ok()?;
    PreviewZoom::LEVELS
        .iter()
        .copied()
        .find(|zoom| (zoom.factor() * 100.0).round() as u32 == percent)
}

fn appearance_id(appearance: PreviewAppearance) -> &'static str {
    match appearance {
        PreviewAppearance::System => "system",
        PreviewAppearance::Light => "light",
        PreviewAppearance::Dark => "dark",
    }
}

fn appearance_choices() -> Vec<SettingChoice> {
    vec![
        choice("system", "System", Some("Follow the operating system.")),
        choice("light", "Light", None),
        choice("dark", "Dark", None),
    ]
}

fn appearance_from_id(id: &str) -> Option<PreviewAppearance> {
    Some(match id {
        "system" => PreviewAppearance::System,
        "light" => PreviewAppearance::Light,
        "dark" => PreviewAppearance::Dark,
        _ => return None,
    })
}

fn link_target_id(target: BrowserLinkTarget) -> &'static str {
    match target {
        BrowserLinkTarget::System => "system",
        BrowserLinkTarget::App => "app",
    }
}

fn link_target_choices() -> Vec<SettingChoice> {
    vec![
        choice(
            "system",
            "Your default browser",
            Some("Open links outside the app."),
        ),
        choice(
            "app",
            "Remote Agent",
            Some("Open links in the in-app Preview surface."),
        ),
    ]
}

fn link_target_from_id(id: &str) -> Option<BrowserLinkTarget> {
    Some(match id {
        "system" => BrowserLinkTarget::System,
        "app" => BrowserLinkTarget::App,
        _ => return None,
    })
}

fn profile_choices(browser: &BrowserSettings) -> Vec<SettingChoice> {
    browser
        .resolved()
        .profiles
        .into_iter()
        .filter(|profile| profile.kind == BrowserProfileKind::Persistent)
        .map(|profile| choice(&profile.id, &profile.name, None))
        .collect()
}

fn row_with_reset(
    id: SettingId,
    title: &str,
    description: Option<&str>,
    control: SettingControl,
    resettable: bool,
) -> SettingsRow {
    SettingsRow {
        resettable,
        ..row(id, title, description, control)
    }
}

pub(super) const SECTION: Section = Section {
    ids: &[
        SettingId::BrowserDefaultViewport,
        SettingId::BrowserDefaultViewportWidth,
        SettingId::BrowserDefaultViewportHeight,
        SettingId::BrowserDefaultZoom,
        SettingId::BrowserDefaultAppearance,
        SettingId::BrowserRecordingFrameRate,
        SettingId::BrowserRecordingShowKeyPresses,
        SettingId::BrowserRecordingShowMousePresses,
        SettingId::BrowserLinkTarget,
        SettingId::BrowserAutoShowFloatingPreview,
        SettingId::BrowserDefaultProfile,
        SettingId::BrowserProfiles,
    ],
    host: |context: &Context| {
        let browser = &context.snapshot.preferences.browser;
        let resolved = browser.resolved();
        let defaults = Preferences::default().browser;
        let default_resolved = defaults.resolved();
        let mut rows = vec![row_with_reset(
            SettingId::BrowserDefaultViewport,
            "Default browser viewport",
            Some("The size used by newly opened Preview tabs."),
            SettingControl::Choice {
                choices: viewport_choices(),
                selected: Some(viewport_id(&browser.viewport)),
            },
            browser.viewport != defaults.viewport,
        )];
        if let PreviewViewportSetting::Freeform { width, height } = browser.viewport {
            rows.extend([
                row_with_reset(
                    SettingId::BrowserDefaultViewportWidth,
                    "Responsive viewport width",
                    Some("The width used by newly opened responsive Preview tabs."),
                    SettingControl::Number {
                        value: width,
                        min: PREVIEW_VIEWPORT_MIN_DIMENSION,
                        max: PREVIEW_VIEWPORT_MAX_DIMENSION,
                    },
                    browser.viewport != defaults.viewport,
                ),
                row_with_reset(
                    SettingId::BrowserDefaultViewportHeight,
                    "Responsive viewport height",
                    Some("The height used by newly opened responsive Preview tabs."),
                    SettingControl::Number {
                        value: height,
                        min: PREVIEW_VIEWPORT_MIN_DIMENSION,
                        max: PREVIEW_VIEWPORT_MAX_DIMENSION,
                    },
                    browser.viewport != defaults.viewport,
                ),
            ]);
        }
        rows.extend([
            row_with_reset(
                SettingId::BrowserDefaultZoom,
                "Default browser zoom",
                Some("The page scale used by newly opened Preview tabs."),
                SettingControl::Choice {
                    choices: zoom_choices(),
                    selected: Some(zoom_id(browser.zoom)),
                },
                browser.zoom != defaults.zoom,
            ),
            row_with_reset(
                SettingId::BrowserDefaultAppearance,
                "Default browser appearance",
                Some("The color scheme pages are told to prefer."),
                SettingControl::Choice {
                    choices: appearance_choices(),
                    selected: Some(appearance_id(browser.appearance).into()),
                },
                browser.appearance != defaults.appearance,
            ),
            row_with_reset(
                SettingId::BrowserRecordingFrameRate,
                "Browser recording frame rate",
                Some("30 fps saves CPU and storage; 60 fps is smoother."),
                SettingControl::Choice {
                    choices: vec![choice("30", "30 fps", None), choice("60", "60 fps", None)],
                    selected: Some(browser.recording_frame_rate.to_string()),
                },
                browser.recording_frame_rate != defaults.recording_frame_rate,
            ),
            row_with_reset(
                SettingId::BrowserRecordingShowKeyPresses,
                "Show key presses in recordings",
                Some("Show pressed keys and shortcuts in new recordings."),
                SettingControl::Switch {
                    on: browser.recording_show_key_presses,
                },
                browser.recording_show_key_presses != defaults.recording_show_key_presses,
            ),
            row_with_reset(
                SettingId::BrowserRecordingShowMousePresses,
                "Show mouse presses in recordings",
                Some("Highlight mouse presses and held buttons in new recordings."),
                SettingControl::Switch {
                    on: browser.recording_show_mouse_presses,
                },
                browser.recording_show_mouse_presses != defaults.recording_show_mouse_presses,
            ),
            row_with_reset(
                SettingId::BrowserLinkTarget,
                "Open links in",
                Some("Choose the operating system browser or the in-app Preview surface."),
                SettingControl::Choice {
                    choices: link_target_choices(),
                    selected: Some(link_target_id(browser.link_target).into()),
                },
                browser.link_target != defaults.link_target,
            ),
            row_with_reset(
                SettingId::BrowserAutoShowFloatingPreview,
                "Auto-show floating Preview",
                Some("Show agent-opened Preview tabs without requiring a manual reveal."),
                SettingControl::Switch {
                    on: browser.auto_show_floating_preview,
                },
                browser.auto_show_floating_preview != defaults.auto_show_floating_preview,
            ),
            row_with_reset(
                SettingId::BrowserDefaultProfile,
                "Default browser profile",
                Some("The persistent profile used by newly opened Preview tabs."),
                SettingControl::Choice {
                    choices: profile_choices(browser),
                    selected: Some(resolved.profile_id.clone()),
                },
                browser.default_profile_id != defaults.default_profile_id
                    || resolved.profile_id != default_resolved.profile_id,
            ),
            row(
                SettingId::BrowserProfiles,
                "Browser profiles",
                Some("Create, rename, clear, and remove persistent browser identities."),
                SettingControl::BrowserProfiles {
                    profiles: resolved
                        .profiles
                        .iter()
                        .filter(|profile| profile.kind == BrowserProfileKind::Persistent)
                        .cloned()
                        .collect(),
                    default_profile_id: resolved.profile_id.clone(),
                },
            ),
        ]);
        Some(section(
            "browser",
            "Browser",
            rows,
            Some("Browser profiles are device-local; Default and Incognito are always available."),
        ))
    },
    project: |_| None,
    intent: |snapshot, scope, id, value| {
        if !matches!(scope, SettingsScope::Host) {
            return None;
        }
        match (id, value) {
            (SettingId::BrowserDefaultViewport, SettingValue::Choice { id }) => {
                viewport_from_id(id, &snapshot.preferences.browser.viewport)
                    .map(|viewport| Intent::SetBrowserViewport { viewport })
            }
            (SettingId::BrowserDefaultViewportWidth, SettingValue::Number { value }) => {
                match snapshot.preferences.browser.viewport {
                    PreviewViewportSetting::Freeform { height, .. } => {
                        let viewport = PreviewViewportSetting::Freeform {
                            width: *value,
                            height,
                        };
                        crate::view::browser::validate_browser_viewport(&viewport)
                            .ok()
                            .map(|_| Intent::SetBrowserViewport { viewport })
                    }
                    _ => None,
                }
            }
            (SettingId::BrowserDefaultViewportHeight, SettingValue::Number { value }) => {
                match snapshot.preferences.browser.viewport {
                    PreviewViewportSetting::Freeform { width, .. } => {
                        let viewport = PreviewViewportSetting::Freeform {
                            width,
                            height: *value,
                        };
                        crate::view::browser::validate_browser_viewport(&viewport)
                            .ok()
                            .map(|_| Intent::SetBrowserViewport { viewport })
                    }
                    _ => None,
                }
            }
            (SettingId::BrowserDefaultZoom, SettingValue::Choice { id }) => {
                zoom_from_id(id).map(|zoom| Intent::SetBrowserZoom { zoom })
            }
            (SettingId::BrowserDefaultAppearance, SettingValue::Choice { id }) => {
                appearance_from_id(id).map(|appearance| Intent::SetBrowserAppearance { appearance })
            }
            (SettingId::BrowserRecordingFrameRate, SettingValue::Choice { id }) => id
                .parse::<u32>()
                .ok()
                .filter(|frame_rate| {
                    crate::view::browser::BROWSER_RECORDING_FRAME_RATES.contains(frame_rate)
                })
                .map(|frame_rate| Intent::SetBrowserRecordingFrameRate { frame_rate }),
            (SettingId::BrowserRecordingShowKeyPresses, SettingValue::Switch { on }) => {
                Some(Intent::SetBrowserRecordingShowKeyPresses { enabled: *on })
            }
            (SettingId::BrowserRecordingShowMousePresses, SettingValue::Switch { on }) => {
                Some(Intent::SetBrowserRecordingShowMousePresses { enabled: *on })
            }
            (SettingId::BrowserLinkTarget, SettingValue::Choice { id }) => {
                link_target_from_id(id).map(|target| Intent::SetBrowserLinkTarget { target })
            }
            (SettingId::BrowserAutoShowFloatingPreview, SettingValue::Switch { on }) => {
                Some(Intent::SetBrowserAutoShowFloatingPreview { enabled: *on })
            }
            (SettingId::BrowserDefaultProfile, SettingValue::Choice { id }) => snapshot
                .preferences
                .browser
                .resolved()
                .profiles
                .iter()
                .find(|profile| profile.id == *id && profile.kind == BrowserProfileKind::Persistent)
                .map(|profile| Intent::SetBrowserDefaultProfile {
                    profile_id: profile.id.clone(),
                }),
            (SettingId::BrowserProfiles, SettingValue::Choice { id }) => snapshot
                .preferences
                .browser
                .resolved()
                .profiles
                .iter()
                .find(|profile| profile.id == *id && profile.kind == BrowserProfileKind::Persistent)
                .map(|profile| Intent::SetBrowserDefaultProfile {
                    profile_id: profile.id.clone(),
                }),
            _ => None,
        }
    },
    reset: |id| {
        let defaults = Preferences::default().browser;
        match id {
            SettingId::BrowserDefaultViewport => Some(Intent::SetBrowserViewport {
                viewport: defaults.viewport,
            }),
            SettingId::BrowserDefaultViewportWidth | SettingId::BrowserDefaultViewportHeight => {
                Some(Intent::SetBrowserViewport {
                    viewport: defaults.viewport,
                })
            }
            SettingId::BrowserDefaultZoom => Some(Intent::SetBrowserZoom {
                zoom: defaults.zoom,
            }),
            SettingId::BrowserDefaultAppearance => Some(Intent::SetBrowserAppearance {
                appearance: defaults.appearance,
            }),
            SettingId::BrowserRecordingFrameRate => Some(Intent::SetBrowserRecordingFrameRate {
                frame_rate: defaults.recording_frame_rate,
            }),
            SettingId::BrowserRecordingShowKeyPresses => {
                Some(Intent::SetBrowserRecordingShowKeyPresses {
                    enabled: defaults.recording_show_key_presses,
                })
            }
            SettingId::BrowserRecordingShowMousePresses => {
                Some(Intent::SetBrowserRecordingShowMousePresses {
                    enabled: defaults.recording_show_mouse_presses,
                })
            }
            SettingId::BrowserLinkTarget => Some(Intent::SetBrowserLinkTarget {
                target: defaults.link_target,
            }),
            SettingId::BrowserAutoShowFloatingPreview => {
                Some(Intent::SetBrowserAutoShowFloatingPreview {
                    enabled: defaults.auto_show_floating_preview,
                })
            }
            SettingId::BrowserDefaultProfile => Some(Intent::SetBrowserDefaultProfile {
                profile_id: defaults.resolved().profile_id,
            }),
            SettingId::BrowserProfiles => Some(Intent::SetBrowserProfiles { profiles: vec![] }),
            _ => None,
        }
    },
    inherit: |_| None,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Snapshot;
    use crate::view::{settings::settings_view, time::TimestampFormat};

    #[test]
    fn browser_rows_use_device_defaults_and_typed_intents() {
        let mut snapshot = Snapshot::default();
        snapshot.preferences.browser.zoom = PreviewZoom::X125;
        snapshot.preferences.browser.link_target = BrowserLinkTarget::App;
        let view = settings_view(
            &snapshot,
            None,
            &SettingsScope::Host,
            TimestampFormat::Locale,
        );
        let section = view
            .sections
            .iter()
            .find(|section| section.id == "browser")
            .unwrap();
        assert_eq!(section.rows.len(), 10);
        assert_eq!(
            section.rows[1].control,
            SettingControl::Choice {
                choices: zoom_choices(),
                selected: Some("125".into()),
            }
        );
        assert_eq!(
            (SECTION.intent)(
                &snapshot,
                &SettingsScope::Host,
                SettingId::BrowserLinkTarget,
                &SettingValue::Choice { id: "app".into() },
            ),
            Some(Intent::SetBrowserLinkTarget {
                target: BrowserLinkTarget::App,
            })
        );
        assert!(
            (SECTION.intent)(
                &snapshot,
                &SettingsScope::Host,
                SettingId::BrowserRecordingFrameRate,
                &SettingValue::Choice { id: "24".into() },
            )
            .is_none()
        );
    }

    #[test]
    fn responsive_viewport_exposes_bounded_dimensions() {
        let mut snapshot = Snapshot::default();
        snapshot.preferences.browser.viewport = PreviewViewportSetting::Freeform {
            width: 1_024,
            height: 768,
        };
        let view = settings_view(
            &snapshot,
            None,
            &SettingsScope::Host,
            TimestampFormat::Locale,
        );
        let section = view
            .sections
            .iter()
            .find(|section| section.id == "browser")
            .unwrap();
        assert!(matches!(
            section.rows[1].control,
            SettingControl::Number {
                value: 1_024,
                min: PREVIEW_VIEWPORT_MIN_DIMENSION,
                max: PREVIEW_VIEWPORT_MAX_DIMENSION,
            }
        ));
        assert!(matches!(
            (SECTION.intent)(
                &snapshot,
                &SettingsScope::Host,
                SettingId::BrowserDefaultViewportWidth,
                &SettingValue::Number { value: 1_200 },
            ),
            Some(Intent::SetBrowserViewport {
                viewport: PreviewViewportSetting::Freeform {
                    width: 1_200,
                    height: 768,
                }
            })
        ));
        assert!(
            (SECTION.intent)(
                &snapshot,
                &SettingsScope::Host,
                SettingId::BrowserDefaultViewportWidth,
                &SettingValue::Number { value: 10 },
            )
            .is_none()
        );
    }
}
