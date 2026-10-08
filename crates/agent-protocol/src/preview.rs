//! Preview contracts shared by the Host and desktop clients.
//!
//! A preview is a Host owned browser tab.  The client owns presentation and
//! input while the Host owns the tab, navigation metadata and local server
//! discovery.
use serde::{Deserialize, Serialize};

pub const PREVIEW_URL_MAX_LENGTH: usize = 2_048;
pub const PREVIEW_TITLE_MAX_LENGTH: usize = 512;
pub const CONFIGURED_LOCAL_SERVER_URLS_MAX_ITEMS: usize = 32;
pub const PREVIEW_VIEWPORT_MIN_DIMENSION: u32 = 240;
pub const PREVIEW_VIEWPORT_MAX_DIMENSION: u32 = 3_840;
pub const PREVIEW_VIEWPORT_MAX_AREA: u64 = 3_840 * 2_160;
pub const PREVIEW_RECORDING_MAX_BYTES: u64 = 50 * 1024 * 1024;
pub const PREVIEW_RECORDING_MAX_DURATION_SECONDS: u64 = 120;
pub const PREVIEW_PROFILE_ID_MAX_LENGTH: usize = 64;
pub const DEFAULT_PREVIEW_PROFILE_ID: &str = "default";
pub const INCOGNITO_PREVIEW_PROFILE_ID: &str = "incognito";

pub const COMMON_DEV_PORTS: &[u16] = &[
    3000, 3001, 3333, 4173, 4200, 4321, 5000, 5173, 5174, 5175, 5500, 8000, 8080, 8081, 8888,
    9000,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreviewAppearance {
    System,
    Light,
    Dark,
}
impl Default for PreviewAppearance {
    fn default() -> Self {
        Self::System
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PreviewViewportPreset {
    IphoneSe,
    IphoneXr,
    #[serde(rename = "iphone-12-pro")]
    Iphone12Pro,
    #[serde(rename = "iphone-14-pro-max")]
    Iphone14ProMax,
    #[serde(rename = "pixel-7")]
    Pixel7,
    #[serde(rename = "samsung-galaxy-s8-plus")]
    SamsungGalaxyS8Plus,
    #[serde(rename = "samsung-galaxy-s20-ultra")]
    SamsungGalaxyS20Ultra,
    #[serde(rename = "ipad-mini")]
    IpadMini,
    #[serde(rename = "ipad-air")]
    IpadAir,
    #[serde(rename = "ipad-pro")]
    IpadPro,
    #[serde(rename = "surface-pro-7")]
    SurfacePro7,
    #[serde(rename = "surface-duo")]
    SurfaceDuo,
    #[serde(rename = "galaxy-z-fold-5")]
    GalaxyZFold5,
    #[serde(rename = "asus-zenbook-fold")]
    AsusZenbookFold,
    #[serde(rename = "samsung-galaxy-a51-71")]
    SamsungGalaxyA5171,
    NestHub,
    #[serde(rename = "nest-hub-max")]
    NestHubMax,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewViewportPresetDefinition {
    pub id: PreviewViewportPreset,
    pub label: &'static str,
    pub width: u32,
    pub height: u32,
}

pub const PREVIEW_VIEWPORT_PRESETS: &[PreviewViewportPresetDefinition] = &[
    p(PreviewViewportPreset::IphoneSe, "iPhone SE", 375, 667),
    p(PreviewViewportPreset::IphoneXr, "iPhone XR", 414, 896),
    p(PreviewViewportPreset::Iphone12Pro, "iPhone 12 Pro", 390, 844),
    p(PreviewViewportPreset::Iphone14ProMax, "iPhone 14 Pro Max", 430, 932),
    p(PreviewViewportPreset::Pixel7, "Pixel 7", 412, 915),
    p(PreviewViewportPreset::SamsungGalaxyS8Plus, "Samsung Galaxy S8+", 360, 740),
    p(PreviewViewportPreset::SamsungGalaxyS20Ultra, "Samsung Galaxy S20 Ultra", 412, 915),
    p(PreviewViewportPreset::IpadMini, "iPad Mini", 768, 1024),
    p(PreviewViewportPreset::IpadAir, "iPad Air", 820, 1180),
    p(PreviewViewportPreset::IpadPro, "iPad Pro", 1024, 1366),
    p(PreviewViewportPreset::SurfacePro7, "Surface Pro 7", 912, 1368),
    p(PreviewViewportPreset::SurfaceDuo, "Surface Duo", 540, 720),
    p(PreviewViewportPreset::GalaxyZFold5, "Galaxy Z Fold 5", 344, 882),
    p(PreviewViewportPreset::AsusZenbookFold, "Asus Zenbook Fold", 853, 1280),
    p(PreviewViewportPreset::SamsungGalaxyA5171, "Samsung Galaxy A51/71", 412, 914),
    p(PreviewViewportPreset::NestHub, "Nest Hub", 1024, 600),
    p(PreviewViewportPreset::NestHubMax, "Nest Hub Max", 1280, 800),
];

const fn p(
    id: PreviewViewportPreset,
    label: &'static str,
    width: u32,
    height: u32,
) -> PreviewViewportPresetDefinition {
    PreviewViewportPresetDefinition {
        id,
        label,
        width,
        height,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "_tag", rename_all = "lowercase")]
pub enum PreviewViewportSetting {
    Fill,
    Freeform { width: u32, height: u32 },
    Preset {
        #[serde(rename = "presetId")]
        preset: PreviewViewportPreset,
        width: u32,
        height: u32,
    },
}
impl Default for PreviewViewportSetting {
    fn default() -> Self {
        Self::Fill
    }
}
impl PreviewViewportSetting {
    pub fn dimensions(self) -> Option<(u32, u32)> {
        match self {
            Self::Fill => None,
            Self::Freeform { width, height } | Self::Preset { width, height, .. } => {
                Some((width, height))
            }
        }
    }
    pub fn validate(self) -> Result<(), String> {
        let Some((width, height)) = self.dimensions() else {
            return Ok(());
        };
        validate_viewport_dimensions(width, height)
    }
}

pub fn validate_viewport_dimensions(width: u32, height: u32) -> Result<(), String> {
    if !(PREVIEW_VIEWPORT_MIN_DIMENSION..=PREVIEW_VIEWPORT_MAX_DIMENSION).contains(&width)
        || !(PREVIEW_VIEWPORT_MIN_DIMENSION..=PREVIEW_VIEWPORT_MAX_DIMENSION).contains(&height)
    {
        return Err(format!(
            "preview viewport dimensions must be {PREVIEW_VIEWPORT_MIN_DIMENSION} to {PREVIEW_VIEWPORT_MAX_DIMENSION}"
        ));
    }
    if u64::from(width) * u64::from(height) > PREVIEW_VIEWPORT_MAX_AREA {
        return Err(format!(
            "preview viewport area must not exceed {PREVIEW_VIEWPORT_MAX_AREA} pixels"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewZoom {
    X25,
    X33,
    X50,
    X67,
    X75,
    X80,
    X90,
    X100,
    X110,
    X125,
    X150,
    X175,
    X200,
    X250,
    X300,
    X400,
    X500,
}
impl Serialize for PreviewZoom {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_f32(self.factor())
    }
}
impl<'de> Deserialize<'de> for PreviewZoom {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = f32::deserialize(deserializer)?;
        Self::LEVELS
            .iter()
            .copied()
            .find(|level| (level.factor() - value).abs() < f32::EPSILON)
            .ok_or_else(|| serde::de::Error::custom("unsupported preview zoom"))
    }
}
impl PreviewZoom {
    pub const LEVELS: &'static [Self] = &[
        Self::X25,
        Self::X33,
        Self::X50,
        Self::X67,
        Self::X75,
        Self::X80,
        Self::X90,
        Self::X100,
        Self::X110,
        Self::X125,
        Self::X150,
        Self::X175,
        Self::X200,
        Self::X250,
        Self::X300,
        Self::X400,
        Self::X500,
    ];
    pub const fn factor(self) -> f32 {
        match self {
            Self::X25 => 0.25,
            Self::X33 => 0.33,
            Self::X50 => 0.50,
            Self::X67 => 0.67,
            Self::X75 => 0.75,
            Self::X80 => 0.80,
            Self::X90 => 0.90,
            Self::X100 => 1.0,
            Self::X110 => 1.10,
            Self::X125 => 1.25,
            Self::X150 => 1.50,
            Self::X175 => 1.75,
            Self::X200 => 2.0,
            Self::X250 => 2.50,
            Self::X300 => 3.0,
            Self::X400 => 4.0,
            Self::X500 => 5.0,
        }
    }
    pub fn stepped(self, direction: i8) -> Self {
        let index = Self::LEVELS.iter().position(|value| *value == self).unwrap_or(7);
        let next = if direction < 0 {
            index.saturating_sub(1)
        } else if direction > 0 {
            (index + 1).min(Self::LEVELS.len() - 1)
        } else {
            index
        };
        Self::LEVELS[next]
    }
}
impl Default for PreviewZoom {
    fn default() -> Self {
        Self::X100
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "_tag", rename_all = "PascalCase")]
pub enum PreviewNavStatus {
    Idle,
    Loading { url: String, title: String },
    Success { url: String, title: String },
    LoadFailed {
        url: String,
        title: String,
        code: i32,
        description: String,
    },
}
impl Default for PreviewNavStatus {
    fn default() -> Self {
        Self::Idle
    }
}
impl PreviewNavStatus {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Idle => Ok(()),
            Self::Loading { url, title }
            | Self::Success { url, title }
            | Self::LoadFailed { url, title, .. } => {
                normalize_preview_url(url).map(|_| ())?;
                if title.chars().count() > PREVIEW_TITLE_MAX_LENGTH {
                    return Err("preview title is too long".into());
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSessionSnapshot {
    pub thread_id: agent_domain::ThreadId,
    pub tab_id: String,
    pub nav_status: PreviewNavStatus,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub viewport: PreviewViewportSetting,
    pub zoom: PreviewZoom,
    pub appearance: PreviewAppearance,
    /// The isolated browser identity used to create this tab.  The built-in
    /// default profile is omitted so older snapshots remain compact; Hosts
    /// still treat an omitted value as the default partition.
    pub profile_id: Option<String>,
    pub updated_at: String,
}
impl PreviewSessionSnapshot {
    pub fn validate(&self) -> Result<(), String> {
        if self.tab_id.trim().is_empty() || self.tab_id.len() > 128 {
            return Err("preview tab id is invalid".into());
        }
        if let Some(profile_id) = &self.profile_id {
            validate_profile_id(profile_id)?;
        }
        self.viewport.validate()?;
        self.nav_status.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredLocalServer {
    pub host: String,
    pub port: u16,
    pub url: String,
    pub process_name: Option<String>,
    pub pid: Option<u32>,
    pub terminal: Option<PreviewTerminalOwner>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewTerminalOwner {
    pub thread_id: agent_domain::ThreadId,
    pub terminal_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewListResult {
    pub sessions: Vec<PreviewSessionSnapshot>,
    pub recordings: Vec<PreviewRecordingStatus>,
    /// Completed artifacts that were evicted by the Host retention owner.
    /// Clients must drop any Save/Attach reference for these tabs.
    pub invalidated_recordings: Vec<String>,
    pub local_servers: Vec<DiscoveredLocalServer>,
    pub scanned_at: String,
    pub server_epoch: String,
    pub revision: u64,
    /// The scanner's process epoch and change revision are independent from
    /// browser-tab metadata, because port polling can change without tabs.
    pub scanner_epoch: String,
    pub scanner_revision: u64,
}

/// The measured guest viewport supplied by a Fill-mode owner. It has a
/// separate contract from selectable viewport sizes because a narrow panel is
/// allowed to be smaller than the minimum fixed device preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRenderedViewportSize {
    pub width: u32,
    pub height: u32,
}
impl PreviewRenderedViewportSize {
    pub fn validate(self) -> Result<(), String> {
        if self.width == 0 || self.height == 0 {
            Err("measured preview viewport dimensions must be positive".into())
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewList {
    pub thread_id: agent_domain::ThreadId,
    pub configured_urls: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRecordingStart {
    pub thread_id: agent_domain::ThreadId,
    pub tab_id: String,
    pub options: PreviewRecordingOptions,
}
impl PreviewRecordingStart {
    pub fn validate(&self) -> Result<(), String> {
        validate_tab_id(&self.tab_id)?;
        self.options.validate()
    }
}

/// Client-owned recording preferences forwarded to the Host capture owner.
/// The Host never silently substitutes a different frame rate or decoration
/// policy after a recording has started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRecordingOptions {
    pub frame_rate: u8,
    pub show_key_presses: bool,
    pub show_mouse_presses: bool,
}
impl Default for PreviewRecordingOptions {
    fn default() -> Self {
        Self {
            frame_rate: 30,
            show_key_presses: false,
            show_mouse_presses: false,
        }
    }
}
impl PreviewRecordingOptions {
    pub fn validate(self) -> Result<(), String> {
        if matches!(self.frame_rate, 30 | 60) {
            Ok(())
        } else {
            Err("preview recording frame rate must be 30 or 60".into())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRecordingStop {
    pub thread_id: agent_domain::ThreadId,
    pub tab_id: String,
}
impl PreviewRecordingStop {
    pub fn validate(&self) -> Result<(), String> {
        validate_tab_id(&self.tab_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRecordingStatus {
    pub tab_id: String,
    pub recording: bool,
    pub started_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRecordingArtifact {
    pub id: String,
    pub tab_id: String,
    pub path: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub created_at: String,
}
impl PreviewList {
    pub fn validate(&self) -> Result<(), String> {
        if self.configured_urls.len() > CONFIGURED_LOCAL_SERVER_URLS_MAX_ITEMS {
            return Err("too many configured preview URLs".into());
        }
        self.configured_urls.iter().try_for_each(|url| {
            if url.len() > PREVIEW_URL_MAX_LENGTH {
                Err("configured preview URL is too long".into())
            } else {
                Ok(())
            }
        })
    }
}

/// The long-lived Preview metadata and local-server subscription. Each stream
/// item is a complete snapshot so a reconnect or scanner update carries the
/// owning Host epoch and revision with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSubscribe {
    pub thread_id: agent_domain::ThreadId,
    pub configured_urls: Vec<String>,
}
impl PreviewSubscribe {
    pub fn validate(&self) -> Result<(), String> {
        PreviewList {
            thread_id: self.thread_id.clone(),
            configured_urls: self.configured_urls.clone(),
        }
        .validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewOpen {
    pub thread_id: agent_domain::ThreadId,
    pub url: Option<String>,
    pub viewport: PreviewViewportSetting,
    pub appearance: PreviewAppearance,
    pub zoom: PreviewZoom,
    pub rendered_size: Option<PreviewRenderedViewportSize>,
    pub profile_id: Option<String>,
}
impl PreviewOpen {
    pub fn validate(&self) -> Result<(), String> {
        self.viewport.validate()?;
        if let Some(size) = self.rendered_size {
            size.validate()?;
        }
        if let Some(url) = &self.url {
            normalize_preview_url(url).map(|_| ())?;
        }
        if let Some(profile_id) = &self.profile_id {
            validate_profile_id(profile_id)?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewNavigate {
    pub thread_id: agent_domain::ThreadId,
    pub tab_id: String,
    pub url: String,
}
impl PreviewNavigate {
    pub fn validate(&self) -> Result<(), String> {
        if self.tab_id.trim().is_empty() || self.tab_id.len() > 128 {
            return Err("preview tab id is invalid".into());
        }
        normalize_preview_url(&self.url).map(|_| ())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewResize {
    pub thread_id: agent_domain::ThreadId,
    pub tab_id: String,
    pub viewport: PreviewViewportSetting,
    pub rendered_size: Option<PreviewRenderedViewportSize>,
}
impl PreviewResize {
    pub fn validate(&self) -> Result<(), String> {
        if self.tab_id.trim().is_empty() || self.tab_id.len() > 128 {
            return Err("preview tab id is invalid".into());
        }
        self.viewport.validate()?;
        if let Some(size) = self.rendered_size {
            size.validate()?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSetAppearance {
    pub thread_id: agent_domain::ThreadId,
    pub tab_id: String,
    pub appearance: PreviewAppearance,
}
impl PreviewSetAppearance {
    pub fn validate(&self) -> Result<(), String> {
        validate_tab_id(&self.tab_id)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSetZoom {
    pub thread_id: agent_domain::ThreadId,
    pub tab_id: String,
    pub zoom: PreviewZoom,
}
impl PreviewSetZoom {
    pub fn validate(&self) -> Result<(), String> {
        validate_tab_id(&self.tab_id)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewClose {
    pub thread_id: agent_domain::ThreadId,
    pub tab_id: Option<String>,
}
impl PreviewClose {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(tab_id) = &self.tab_id {
            validate_tab_id(tab_id)?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewTab {
    pub thread_id: agent_domain::ThreadId,
    pub tab_id: String,
}
impl PreviewTab {
    pub fn validate(&self) -> Result<(), String> {
        validate_tab_id(&self.tab_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewReportStatus {
    pub thread_id: agent_domain::ThreadId,
    pub tab_id: String,
    pub nav_status: PreviewNavStatus,
    pub can_go_back: bool,
    pub can_go_forward: bool,
}
impl PreviewReportStatus {
    pub fn validate(&self) -> Result<(), String> {
        validate_tab_id(&self.tab_id)?;
        self.nav_status.validate()
    }
}

fn validate_tab_id(tab_id: &str) -> Result<(), String> {
    if tab_id.trim().is_empty() || tab_id.len() > 128 {
        Err("preview tab id is invalid".into())
    } else {
        Ok(())
    }
}

pub fn validate_profile_id(profile_id: &str) -> Result<(), String> {
    if profile_id.trim().is_empty()
        || profile_id.len() > PREVIEW_PROFILE_ID_MAX_LENGTH
        || profile_id.chars().any(char::is_control)
    {
        Err("preview profile id is invalid".into())
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum PreviewEvent {
    Opened {
        thread_id: agent_domain::ThreadId,
        tab_id: String,
        revision: u64,
        server_epoch: String,
        created_at: String,
        snapshot: PreviewSessionSnapshot,
    },
    Navigated {
        thread_id: agent_domain::ThreadId,
        tab_id: String,
        revision: u64,
        server_epoch: String,
        created_at: String,
        snapshot: PreviewSessionSnapshot,
    },
    Resized {
        thread_id: agent_domain::ThreadId,
        tab_id: String,
        revision: u64,
        server_epoch: String,
        created_at: String,
        snapshot: PreviewSessionSnapshot,
    },
    Failed {
        thread_id: agent_domain::ThreadId,
        tab_id: String,
        revision: u64,
        server_epoch: String,
        created_at: String,
        url: String,
        title: String,
        code: i32,
        description: String,
    },
    Closed {
        thread_id: agent_domain::ThreadId,
        tab_id: String,
        revision: u64,
        server_epoch: String,
        created_at: String,
    },
    RecordingChanged {
        thread_id: agent_domain::ThreadId,
        tab_id: String,
        revision: u64,
        server_epoch: String,
        created_at: String,
        status: PreviewRecordingStatus,
    },
    RecordingArtifactRemoved {
        thread_id: agent_domain::ThreadId,
        tab_id: String,
        revision: u64,
        server_epoch: String,
        created_at: String,
    },
}

pub fn normalize_preview_url(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() || input.len() > PREVIEW_URL_MAX_LENGTH {
        return Err("preview URL is empty or too long".into());
    }
    let source = if input.contains("://") {
        input.to_owned()
    } else {
        let protocol = if bare_preview_host_is_loopback(input) {
            "http"
        } else {
            "https"
        };
        format!("{protocol}://{input}")
    };
    let mut url = url::Url::parse(&source).map_err(|_| "preview URL is invalid".to_owned())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("preview URL must use http or https".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("preview URL must not contain credentials".into());
    }
    let value = url.to_string();
    if value.len() > PREVIEW_URL_MAX_LENGTH {
        return Err("preview URL is too long".into());
    }
    Ok(value)
}

fn bare_preview_host_is_loopback(input: &str) -> bool {
    let authority = input.split(['/', '?', '#']).next().unwrap_or(input);
    let host = if let Some(end) = authority.strip_prefix('[').and_then(|value| value.find(']')) {
        &authority[1..=end]
    } else {
        authority
            .rsplit_once(':')
            .filter(|(_, port)| port.chars().all(|character| character.is_ascii_digit()))
            .map_or(authority, |(host, _)| host)
    };
    matches!(
        host.to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "0.0.0.0" | "::1"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_viewports_against_the_resource_owned_limits() {
        assert!(PreviewViewportSetting::Freeform { width: 390, height: 844 }.validate().is_ok());
        assert!(PreviewViewportSetting::Freeform { width: 239, height: 844 }.validate().is_err());
        assert!(PreviewViewportSetting::Freeform { width: 3840, height: 2161 }.validate().is_err());
        assert!(PreviewViewportSetting::Fill.validate().is_ok());
    }

    #[test]
    fn measured_fill_dimensions_are_positive_without_using_selectable_preset_bounds() {
        assert!(PreviewRenderedViewportSize { width: 1, height: 1 }.validate().is_ok());
        assert!(PreviewRenderedViewportSize { width: 0, height: 1 }.validate().is_err());
    }

    #[test]
    fn recording_requests_require_a_tab_id() {
        let thread_id = agent_domain::ThreadId::new("thread").unwrap();
        assert!(PreviewRecordingStart { thread_id: thread_id.clone(), tab_id: "tab".into(), options: PreviewRecordingOptions::default() }
            .validate()
            .is_ok());
        assert!(PreviewRecordingStop { thread_id, tab_id: String::new() }
            .validate()
            .is_err());
    }

    #[test]
    fn normalizes_preview_urls_without_accepting_privileged_or_credential_urls() {
        assert_eq!(normalize_preview_url("localhost:5173/docs").unwrap(), "http://localhost:5173/docs");
        assert_eq!(normalize_preview_url("example.com/docs").unwrap(), "https://example.com/docs");
        assert_eq!(normalize_preview_url("http://localhost:5173/docs#old").unwrap(), "http://localhost:5173/docs#old");
        for url in ["", "file:///tmp/a", "javascript:alert(1)", "https://user:pass@example.com"] {
            assert!(normalize_preview_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn zoom_stepping_clamps_to_the_ladder() {
        assert_eq!(PreviewZoom::X25.stepped(-1), PreviewZoom::X25);
        assert_eq!(PreviewZoom::X100.stepped(1), PreviewZoom::X110);
        assert_eq!(PreviewZoom::X500.stepped(1), PreviewZoom::X500);
    }

    #[test]
    fn viewport_and_zoom_json_match_shared_preview_contract() {
        let viewport = PreviewViewportSetting::Preset {
            preset: PreviewViewportPreset::Iphone12Pro,
            width: 390,
            height: 844,
        };
        assert_eq!(
            serde_json::to_value(viewport).unwrap(),
            serde_json::json!({
                "_tag": "preset",
                "presetId": "iphone-12-pro",
                "width": 390,
                "height": 844,
            })
        );
        assert_eq!(
            serde_json::to_value(PreviewZoom::X125).unwrap(),
            serde_json::json!(1.25)
        );
    }
}
