//! Device discovery, lifecycle, control and frame-stream contracts.
//!
//! The Host owns the simulator/emulator toolchain and the helper processes.
//! Clients only receive bounded records and send typed actions; there is no
//! general command endpoint in this module.
use agent_domain::ThreadId;
use serde::{Deserialize, Serialize};

pub const LOCAL_DEVICE_HOST_ID: &str = "local";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DevicePlatform {
    Ios,
    Android,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceHostKind {
    Local,
    Ssh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceHostStatus {
    Disabled,
    Idle,
    Installing,
    Starting,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceHostConfig {
    pub id: String,
    pub label: String,
    pub target: String,
    pub identity_file: Option<String>,
    pub port: Option<u16>,
}

impl DeviceHostConfig {
    pub fn normalized(mut self) -> Self {
        self.id = self.id.trim().to_owned();
        self.label = self.label.trim().to_owned();
        self.target = self.target.trim().to_owned();
        self.identity_file = self.identity_file.map(|path| path.trim().to_owned());
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        let id = self.id.trim();
        let label = self.label.trim();
        let target = self.target.trim();
        if id.is_empty() || id.len() > 128 {
            return Err("device host id must be 1 to 128 characters".into());
        }
        if id == LOCAL_DEVICE_HOST_ID
            || !id.bytes().next().is_some_and(|byte| byte.is_ascii_alphanumeric())
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Err("device host id is invalid".into());
        }
        if label.is_empty()
            || target.is_empty()
            || self.identity_file.as_deref().is_some_and(|path| path.trim().is_empty())
        {
            return Err("device host label and target are required".into());
        }
        if target.chars().any(char::is_whitespace) || target.starts_with('-') {
            return Err("device host target is invalid".into());
        }
        if self.port.is_some_and(|port| port == 0) {
            return Err("device host port is invalid".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevicePlatformAvailability {
    pub platform: DevicePlatform,
    pub available: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceToolVersion {
    pub required_version: String,
    pub installed_versions: Vec<String>,
    pub running_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceToolVersions {
    pub hub: DeviceToolVersion,
    pub agent: DeviceToolVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceHostSummary {
    pub id: String,
    pub kind: DeviceHostKind,
    pub label: String,
    /// SSH connection fields are returned so native settings can edit the
    /// host without reconstructing or hiding the Host-owned configuration.
    /// Local hosts leave these unset.
    pub target: Option<String>,
    pub identity_file: Option<String>,
    pub port: Option<u16>,
    pub platforms: Vec<DevicePlatformAvailability>,
    pub tools: Option<DeviceToolVersions>,
    pub tool_inspection_error: Option<String>,
    pub hub_installed: bool,
    pub agent_device_installed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSummary {
    pub host_id: String,
    pub id: String,
    pub platform: DevicePlatform,
    pub name: String,
    pub version: String,
    pub booted: bool,
    pub physical: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSession {
    pub thread_id: ThreadId,
    pub host_id: String,
    pub device_id: String,
    pub platform: DevicePlatform,
    pub opened_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceHostStatusRecord {
    pub status: DeviceHostStatus,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootingDevice {
    pub device: DeviceSummary,
    pub thread_id: ThreadId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceServiceState {
    pub supports_host_retry: bool,
    pub supports_tool_update: bool,
    pub supports_tool_inspection: bool,
    pub hosts: Vec<DeviceHostSummary>,
    pub host_status: DeviceHostStatus,
    pub host_status_detail: Option<String>,
    pub host_statuses: std::collections::BTreeMap<String, DeviceHostStatusRecord>,
    pub devices: Vec<DeviceSummary>,
    pub sessions: Vec<DeviceSession>,
    pub booting_devices: Vec<BootingDevice>,
    pub onboarding_completed: bool,
    pub agent_access_enabled: bool,
    pub hub_base_path: String,
    pub revision: u64,
}

impl Default for DeviceServiceState {
    fn default() -> Self {
        Self {
            supports_host_retry: true,
            supports_tool_update: true,
            supports_tool_inspection: true,
            hosts: vec![],
            host_status: DeviceHostStatus::Disabled,
            host_status_detail: None,
            host_statuses: std::collections::BTreeMap::new(),
            devices: vec![],
            sessions: vec![],
            booting_devices: vec![],
            onboarding_completed: false,
            agent_access_enabled: false,
            hub_base_path: "/api/device-hub".into(),
            revision: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceTool {
    Hub,
    Agent,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceListInput {
    pub update_tool: Option<DeviceTool>,
    pub inspect_only: bool,
    pub retry_host_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceConfigureInput {
    pub enabled: Option<bool>,
    pub agent_access_enabled: Option<bool>,
    pub onboarding_completed: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceHostsInput {
    pub hosts: Vec<DeviceHostConfig>,
}

impl DeviceHostsInput {
    pub fn validate(&self) -> Result<(), String> {
        let mut ids = std::collections::BTreeSet::new();
        for host in &self.hosts {
            host.validate()?;
            if !ids.insert(host.id.clone()) {
                return Err("device host ids must be unique".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceOpenInput {
    pub thread_id: ThreadId,
    pub host_id: Option<String>,
    pub device_id: String,
    pub platform: DevicePlatform,
    pub boot: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCloseInput {
    pub thread_id: ThreadId,
    pub host_id: Option<String>,
    pub device_id: Option<String>,
    pub shutdown: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceShutdownInput {
    pub host_id: Option<String>,
    pub device_id: String,
    pub platform: DevicePlatform,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceAppearance {
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceTextSize {
    Small,
    Default,
    Large,
    ExtraLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceColorFilter {
    None,
    Grayscale,
    RedGreen,
    GreenRed,
    BlueYellow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceOrientation {
    Portrait,
    LandscapeLeft,
    PortraitUpsideDown,
    LandscapeRight,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceSettings {
    pub appearance: Option<DeviceAppearance>,
    pub text_size: Option<DeviceTextSize>,
    pub reduce_motion: Option<bool>,
    pub increase_contrast: Option<bool>,
    pub reduce_transparency: Option<bool>,
    pub show_borders: Option<bool>,
    pub voice_over: Option<bool>,
    pub liquid_glass: Option<String>,
    pub color_filter: Option<DeviceColorFilter>,
    pub network_enabled: Option<bool>,
    pub location: Option<(f64, f64)>,
}

impl Default for DeviceSettings {
    fn default() -> Self {
        Self {
            appearance: None,
            text_size: None,
            reduce_motion: None,
            increase_contrast: None,
            reduce_transparency: None,
            show_borders: None,
            voice_over: None,
            liquid_glass: None,
            color_filter: None,
            network_enabled: None,
            location: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceForegroundApp {
    pub id: String,
    pub name: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceDetail {
    pub host_id: String,
    pub device_id: String,
    pub settings: DeviceSettings,
    pub foreground_app: Option<DeviceForegroundApp>,
    pub read_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DevicePermission {
    Camera,
    Microphone,
    Photos,
    Contacts,
    Calendar,
    Reminders,
    Location,
    Notifications,
    Motion,
    MediaLibrary,
    FaceId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DevicePermissionDecision {
    Grant,
    Revoke,
    Reset,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DeviceActionKind {
    SetAppearance(DeviceAppearance),
    SetTextSize(DeviceTextSize),
    SetToggle { setting: String, value: bool },
    SetLiquidGlass(String),
    SetColorFilter(DeviceColorFilter),
    SetOrientation(DeviceOrientation),
    SetLocation { latitude: f64, longitude: f64 },
    ClearLocation,
    SetPermission {
        app_id: String,
        permission: DevicePermission,
        decision: DevicePermissionDecision,
    },
    OpenUrl(String),
    LaunchApp(String),
    TerminateApp(String),
    Shake,
    SendPush {
        app_id: String,
        payload: serde_json::Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceActionInput {
    pub host_id: Option<String>,
    pub device_id: String,
    pub action: DeviceActionKind,
}

impl DeviceActionInput {
    pub fn validate(&self) -> Result<(), String> {
        if self.device_id.trim().is_empty() || self.device_id.len() > 256 {
            return Err("device id is invalid".into());
        }
        match &self.action {
            DeviceActionKind::SetToggle { setting, .. } if setting.trim().is_empty() => {
                Err("device setting is required".into())
            }
            DeviceActionKind::SetToggle { setting, .. }
                if !matches!(
                    setting.as_str(),
                    "reduceMotion"
                        | "increaseContrast"
                        | "reduceTransparency"
                        | "showBorders"
                        | "voiceOver"
                        | "networkEnabled"
                ) =>
            {
                Err("device setting is unsupported".into())
            }
            DeviceActionKind::SetLiquidGlass(value)
                if !matches!(value.as_str(), "clear" | "tinted") =>
            {
                Err("device liquid-glass value is invalid".into())
            }
            DeviceActionKind::SetLocation { latitude, longitude }
                if !latitude.is_finite()
                    || !longitude.is_finite()
                    || !(-90.0..=90.0).contains(latitude)
                    || !(-180.0..=180.0).contains(longitude) =>
            {
                Err("device location is invalid".into())
            }
            DeviceActionKind::OpenUrl(url) if url.trim().is_empty() || url.len() > 8192 => {
                Err("device URL is invalid".into())
            }
            DeviceActionKind::LaunchApp(app)
            | DeviceActionKind::TerminateApp(app)
            | DeviceActionKind::SetLiquidGlass(app)
                if app.trim().is_empty() || app.len() > 512 =>
            {
                Err("device action value is invalid".into())
            }
            DeviceActionKind::SetPermission { app_id, .. }
                if app_id.trim().is_empty() || app_id.len() > 512 =>
            {
                Err("device application id is invalid".into())
            }
            DeviceActionKind::SendPush { app_id, payload }
                if app_id.trim().is_empty()
                    || app_id.len() > 512
                    || (!payload.is_string() && !payload.is_object())
                    || payload.to_string().len() > 64 * 1024 =>
            {
                Err("device push payload is invalid".into())
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceDetailInput {
    pub host_id: Option<String>,
    pub device_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceScreenshotInput {
    pub host_id: Option<String>,
    pub device_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceScreenshot {
    pub device: DeviceSummary,
    #[serde(with = "crate::protocol::bytes")]
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceFrame {
    pub thread_id: ThreadId,
    pub device: DeviceSummary,
    #[serde(with = "crate::protocol::bytes")]
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceEvent {
    State(DeviceServiceState),
    Frame(DeviceFrame),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSubscribeInput {
    pub thread_id: ThreadId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceToolListResult {
    pub host_statuses: std::collections::BTreeMap<String, DeviceHostStatusRecord>,
    pub hosts: Vec<DeviceHostSummary>,
    pub devices: Vec<DeviceSummary>,
    pub open: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceToolOpenResult {
    pub device: DeviceSummary,
    pub command: String,
    pub target_args: Vec<String>,
    pub quick_start: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceToolTargetInput {
    pub host_id: Option<String>,
    pub device_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceToolScreenshotResult {
    pub device: DeviceSummary,
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceToolCloseInput {
    pub host_id: Option<String>,
    pub device_id: Option<String>,
    pub shutdown: bool,
}

pub fn agent_device_target_args(device: &DeviceSummary) -> Vec<String> {
    match device.platform {
        DevicePlatform::Ios => vec!["--platform".into(), "ios".into(), "--udid".into(), device.id.clone()],
        DevicePlatform::Android => vec!["--platform".into(), "android".into(), "--serial".into(), device.id.clone()],
    }
}

pub fn png_dimensions(png: &[u8]) -> (u32, u32) {
    if png.len() < 24
        || png[0..8] != [137, 80, 78, 71, 13, 10, 26, 10]
        || &png[12..16] != b"IHDR"
    {
        return (0, 0);
    }
    (
        u32::from_be_bytes(png[16..20].try_into().unwrap()),
        u32::from_be_bytes(png[20..24].try_into().unwrap()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_and_action_validation_rejects_unsafe_values() {
        let bad_host = DeviceHostConfig {
            id: "local".into(),
            label: "host".into(),
            target: "remote".into(),
            identity_file: None,
            port: None,
        };
        assert!(bad_host.validate().is_err());
        assert!(DeviceHostConfig { id: "-remote".into(), ..bad_host.clone() }.validate().is_err());
        assert!(DeviceHostConfig { id: "remote".into(), identity_file: Some(" ".into()), ..bad_host }.validate().is_err());
        assert!(DeviceHostConfig {
            id: " remote ".into(),
            label: " build host ".into(),
            target: " user@example.com ".into(),
            identity_file: Some(" ~/.ssh/id_ed25519 ".into()),
            port: Some(22),
        }
        .validate()
        .is_ok());
        let bad_action = DeviceActionInput {
            host_id: None,
            device_id: "sim".into(),
            action: DeviceActionKind::SetLocation {
                latitude: 91.0,
                longitude: 0.0,
            },
        };
        assert!(bad_action.validate().is_err());
        assert!(DeviceActionInput {
            host_id: None,
            device_id: "sim".into(),
            action: DeviceActionKind::SetToggle {
                setting: "shell".into(),
                value: true,
            },
        }
        .validate()
        .is_err());
        assert!(DeviceActionInput {
            host_id: None,
            device_id: "sim".into(),
            action: DeviceActionKind::SetLiquidGlass("opaque".into()),
        }
        .validate()
        .is_err());
        assert!(DeviceActionInput {
            host_id: None,
            device_id: "sim".into(),
            action: DeviceActionKind::SendPush {
                app_id: "app.example".into(),
                payload: serde_json::json!(["not", "an", "object"]),
            },
        }
        .validate()
        .is_err());
    }

    #[test]
    fn target_and_png_helpers_follow_the_wire_contract() {
        let device = DeviceSummary {
            host_id: "local".into(),
            id: "emulator-1".into(),
            platform: DevicePlatform::Android,
            name: "Pixel".into(),
            version: "Android".into(),
            booted: true,
            physical: false,
        };
        assert_eq!(agent_device_target_args(&device), ["--platform", "android", "--serial", "emulator-1"]);
        let mut png = vec![137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&320u32.to_be_bytes());
        png.extend_from_slice(&640u32.to_be_bytes());
        assert_eq!(png_dimensions(&png), (320, 640));
        assert_eq!(png_dimensions(&[0, 1, 2]), (0, 0));
    }
}
