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
            || !id
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_alphanumeric())
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Err("device host id is invalid".into());
        }
        if label.is_empty()
            || target.is_empty()
            || self
                .identity_file
                .as_deref()
                .is_some_and(|path| path.trim().is_empty())
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
    /// Wall-clock time used for user-facing session history.
    pub opened_at: String,
    /// Opaque generation used to reject queued events from an earlier reopen.
    pub session_epoch: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceFoldPosture {
    Closed,
    Opened,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceDuoPose {
    Closed,
    Book,
    Open,
    Laptop,
    Tent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceDuoPhysical {
    Faceup,
    Facedown,
}

/// The explicit controls accepted by the reference Duo stream.  Keeping the
/// value typed here prevents the Host from becoming a string command tunnel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DeviceDuoCommand {
    Angle { value: f32 },
    Pose { value: DeviceDuoPose },
    Table { value: bool },
    Physical { value: DeviceDuoPhysical },
    Orientation { value: DeviceOrientation },
}

impl DeviceDuoCommand {
    pub fn validate(&self) -> Result<(), String> {
        if let Self::Angle { value } = self
            && (!value.is_finite() || !(0.0..=180.0).contains(value))
        {
            return Err("device Duo angle must be finite and between 0 and 180".into());
        }
        Ok(())
    }
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
    SetToggle {
        setting: String,
        value: bool,
    },
    SetLiquidGlass(String),
    SetColorFilter(DeviceColorFilter),
    SetOrientation(DeviceOrientation),
    SetLocation {
        latitude: f64,
        longitude: f64,
    },
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
    /// Input and display controls are sent through the Host-owned helper
    /// connection. They are explicit variants so clients cannot tunnel an
    /// arbitrary hub route or shell command.
    Input(DeviceInputKind),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceActionInput {
    pub thread_id: ThreadId,
    pub host_id: Option<String>,
    pub device_id: String,
    pub session_epoch: String,
    pub action: DeviceActionKind,
}

impl DeviceActionInput {
    pub fn validate(&self) -> Result<(), String> {
        validate_device_session_epoch(&self.session_epoch)?;
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
            DeviceActionKind::SetLocation {
                latitude,
                longitude,
            } if !latitude.is_finite()
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
            DeviceActionKind::Input(input) => DeviceInput {
                thread_id: self.thread_id.clone(),
                host_id: self.host_id.clone(),
                device_id: self.device_id.clone(),
                session_epoch: self.session_epoch.clone(),
                input: input.clone(),
            }
            .validate(),
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
    pub session_epoch: String,
    pub device: DeviceSummary,
    #[serde(with = "crate::protocol::bytes")]
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub sequence: u64,
}

/// A frame emitted by the device helper's live transport.  `png` frames are
/// retained for still-image capture; live clients must use `payload` and the
/// encoding metadata so H.264/AVCC/SEMU/MJPEG streams are not reduced to
/// screenshots at the Host boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceFrameEncoding {
    AvccDescription,
    H264,
    Mjpeg,
    Jpeg,
    Png,
    Semu,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceVideoFrame {
    pub thread_id: ThreadId,
    pub session_epoch: String,
    pub device: DeviceSummary,
    #[serde(with = "crate::protocol::bytes")]
    pub payload: Vec<u8>,
    pub encoding: DeviceFrameEncoding,
    pub width: u32,
    pub height: u32,
    pub sequence: u64,
    pub timestamp_us: Option<u64>,
    pub keyframe: bool,
    pub screen_id: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceScreenConfig {
    pub thread_id: Option<ThreadId>,
    pub session_epoch: String,
    pub host_id: Option<String>,
    pub device_id: Option<String>,
    pub width: u32,
    pub height: u32,
    pub orientation: DeviceOrientation,
    pub screen_id: Option<u8>,
    pub supports_hinge_angle: bool,
    pub supports_physical_orientation: bool,
    pub hinge_angle: Option<f32>,
    pub hinge_pose: Option<String>,
    pub table_mode: bool,
    pub table_mode_available: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceAccessibilityElement {
    pub id: String,
    pub label: String,
    pub role: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceAccessibilityTree {
    pub host_id: String,
    pub device_id: String,
    pub session_epoch: String,
    pub elements: Vec<DeviceAccessibilityElement>,
    pub errors: Vec<String>,
    pub read_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceForegroundUpdate {
    pub host_id: String,
    pub device_id: String,
    pub session_epoch: String,
    pub app: Option<DeviceForegroundApp>,
    pub received_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceEventLogEntry {
    pub host_id: String,
    pub device_id: String,
    pub session_epoch: String,
    pub id: u64,
    pub timestamp: String,
    pub kind: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DeviceInputKind {
    Touch {
        phase: DeviceTouchPhase,
        x: f32,
        y: f32,
        raw: bool,
    },
    /// A keyboard event carries both its physical code and the platform's
    /// actual key value. iOS uses the code for HID; Android uses the key value
    /// so shifted and non-ASCII input is preserved without client-side
    /// guessing.
    Key {
        code: String,
        key: String,
        down: bool,
        meta: bool,
        ctrl: bool,
    },
    HardwareButton(DeviceHardwareButton),
    Rotate,
    SetOrientation(DeviceOrientation),
    Fold {
        command: DeviceFoldPosture,
    },
    Duo {
        command: DeviceDuoCommand,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceTouchPhase {
    Begin,
    Move,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceHardwareButton {
    Home,
    Back,
    Recents,
    Power,
    AppSwitcher,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceInput {
    pub thread_id: ThreadId,
    pub host_id: Option<String>,
    pub device_id: String,
    pub session_epoch: String,
    pub input: DeviceInputKind,
}

impl DeviceInput {
    pub fn validate(&self) -> Result<(), String> {
        validate_device_session_epoch(&self.session_epoch)?;
        if self.device_id.trim().is_empty() || self.device_id.len() > 256 {
            return Err("device id is invalid".into());
        }
        match &self.input {
            DeviceInputKind::Touch { x, y, .. }
                if !x.is_finite()
                    || !y.is_finite()
                    || !(0.0..=1.0).contains(x)
                    || !(0.0..=1.0).contains(y) =>
            {
                Err("device touch coordinates must be finite and normalized".into())
            }
            DeviceInputKind::Key { code, key, .. }
                if code.trim().is_empty()
                    || code.len() > 64
                    || key.is_empty()
                    || key.len() > 128 =>
            {
                Err("device key code or key value is invalid".into())
            }
            DeviceInputKind::Duo { command } => command.validate(),
            _ => Ok(()),
        }
    }
}

fn validate_device_session_epoch(epoch: &str) -> Result<(), String> {
    if epoch.trim().is_empty() || epoch.len() > 256 || epoch.chars().any(char::is_control) {
        return Err("device session epoch is invalid".into());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceAccessibilityInput {
    pub host_id: Option<String>,
    pub device_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceEventLogInput {
    pub host_id: Option<String>,
    pub device_id: String,
    pub limit: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceRecordingFormat {
    /// H.264 frames finalized as a playable fragmented MP4 attachment.
    Mp4,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRecordingStartInput {
    pub thread_id: ThreadId,
    pub host_id: Option<String>,
    pub device_id: String,
    pub format: DeviceRecordingFormat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRecordingStopInput {
    pub thread_id: ThreadId,
    pub host_id: Option<String>,
    pub device_id: String,
    pub recording_id: u64,
    pub session_epoch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRecordingStatus {
    pub thread_id: ThreadId,
    pub host_id: String,
    pub device_id: String,
    /// Monotonic identity for one recording lifetime on the owning Host.
    pub recording_id: u64,
    /// Device session generation that supplied the recording frames.
    pub session_epoch: String,
    pub format: DeviceRecordingFormat,
    pub file_name: String,
    pub mime_type: String,
    pub active: bool,
    pub started_at: String,
    pub frame_count: u64,
    pub byte_count: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRecording {
    pub status: DeviceRecordingStatus,
    #[serde(with = "crate::protocol::bytes")]
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DeviceEvent {
    State(DeviceServiceState),
    Frame(DeviceFrame),
    Video(DeviceVideoFrame),
    Screen(DeviceScreenConfig),
    Accessibility(DeviceAccessibilityTree),
    Foreground(DeviceForegroundUpdate),
    EventLog(DeviceEventLogEntry),
    Recording(DeviceRecordingStatus),
    RecordingComplete(DeviceRecording),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSubscribeInput {
    pub thread_id: ThreadId,
    /// Native clients request a JPEG transport because they do not share the
    /// browser WebCodecs decoder used by the reference web panel.
    pub prefer_mjpeg: bool,
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
        DevicePlatform::Ios => vec![
            "--platform".into(),
            "ios".into(),
            "--udid".into(),
            device.id.clone(),
        ],
        DevicePlatform::Android => vec![
            "--platform".into(),
            "android".into(),
            "--serial".into(),
            device.id.clone(),
        ],
    }
}

pub fn png_dimensions(png: &[u8]) -> (u32, u32) {
    if png.len() < 24 || png[0..8] != [137, 80, 78, 71, 13, 10, 26, 10] || &png[12..16] != b"IHDR" {
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
        assert!(
            DeviceHostConfig {
                id: "-remote".into(),
                ..bad_host.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            DeviceHostConfig {
                id: "remote".into(),
                identity_file: Some(" ".into()),
                ..bad_host
            }
            .validate()
            .is_err()
        );
        assert!(
            DeviceHostConfig {
                id: " remote ".into(),
                label: " build host ".into(),
                target: " user@example.com ".into(),
                identity_file: Some(" ~/.ssh/id_ed25519 ".into()),
                port: Some(22),
            }
            .validate()
            .is_ok()
        );
        let bad_action = DeviceActionInput {
            thread_id: ThreadId::new("thread").unwrap(),
            host_id: None,
            device_id: "sim".into(),
            session_epoch: "epoch".into(),
            action: DeviceActionKind::SetLocation {
                latitude: 91.0,
                longitude: 0.0,
            },
        };
        assert!(bad_action.validate().is_err());
        assert!(
            DeviceActionInput {
                thread_id: ThreadId::new("thread").unwrap(),
                host_id: None,
                device_id: "sim".into(),
                session_epoch: "epoch".into(),
                action: DeviceActionKind::SetToggle {
                    setting: "shell".into(),
                    value: true,
                },
            }
            .validate()
            .is_err()
        );
        assert!(
            DeviceActionInput {
                thread_id: ThreadId::new("thread").unwrap(),
                host_id: None,
                device_id: "sim".into(),
                session_epoch: "epoch".into(),
                action: DeviceActionKind::SetLiquidGlass("opaque".into()),
            }
            .validate()
            .is_err()
        );
        assert!(
            DeviceActionInput {
                thread_id: ThreadId::new("thread").unwrap(),
                host_id: None,
                device_id: "sim".into(),
                session_epoch: "epoch".into(),
                action: DeviceActionKind::SendPush {
                    app_id: "app.example".into(),
                    payload: serde_json::json!(["not", "an", "object"]),
                },
            }
            .validate()
            .is_err()
        );
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
        assert_eq!(
            agent_device_target_args(&device),
            ["--platform", "android", "--serial", "emulator-1"]
        );
        let mut png = vec![137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&320u32.to_be_bytes());
        png.extend_from_slice(&640u32.to_be_bytes());
        assert_eq!(png_dimensions(&png), (320, 640));
        assert_eq!(png_dimensions(&[0, 1, 2]), (0, 0));
    }

    #[test]
    fn keyboard_input_validates_actual_key_value() {
        let valid = DeviceActionInput {
            host_id: None,
            device_id: "emulator-1".into(),
            action: DeviceActionKind::Input(DeviceInputKind::Key {
                code: "Digit1".into(),
                key: "!".into(),
                down: true,
                meta: false,
                ctrl: false,
            }),
        };
        assert!(valid.validate().is_ok());
        let empty = DeviceActionInput {
            action: DeviceActionKind::Input(DeviceInputKind::Key {
                code: "KeyA".into(),
                key: String::new(),
                down: true,
                meta: false,
                ctrl: false,
            }),
            ..valid.clone()
        };
        assert!(empty.validate().is_err());
    }

    #[test]
    fn keyboard_input_validates_both_physical_code_and_actual_key_value() {
        let valid = DeviceInput {
            thread_id: ThreadId::new("thread").unwrap(),
            host_id: None,
            device_id: "emulator-1".into(),
            session_epoch: "epoch".into(),
            input: DeviceInputKind::Key {
                code: "KeyA".into(),
                key: "A".into(),
                down: true,
                meta: false,
                ctrl: false,
            },
        };
        assert!(valid.validate().is_ok());
        let mut empty_key = valid.clone();
        if let DeviceInputKind::Key { key, .. } = &mut empty_key.input {
            key.clear();
        }
        assert!(empty_key.validate().is_err());
        let mut oversized_key = valid;
        if let DeviceInputKind::Key { key, .. } = &mut oversized_key.input {
            *key = "x".repeat(129);
        }
        assert!(oversized_key.validate().is_err());
    }

    #[test]
    fn duo_input_validates_a_bounded_angle_without_string_commands() {
        let input = DeviceInput {
            thread_id: ThreadId::new("thread").unwrap(),
            host_id: None,
            device_id: "simulator".into(),
            session_epoch: "epoch".into(),
            input: DeviceInputKind::Duo {
                command: DeviceDuoCommand::Angle { value: 90.0 },
            },
        };
        assert!(input.validate().is_ok());
        let mut invalid = input.clone();
        if let DeviceInputKind::Duo { command } = &mut invalid.input {
            *command = DeviceDuoCommand::Angle { value: 180.1 };
        }
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn scoped_device_requests_require_a_non_control_session_epoch() {
        let mut input = DeviceInput {
            thread_id: ThreadId::new("thread").unwrap(),
            host_id: None,
            device_id: "simulator".into(),
            session_epoch: "epoch".into(),
            input: DeviceInputKind::Rotate,
        };
        assert!(input.validate().is_ok());
        input.session_epoch = " \n".into();
        assert!(input.validate().is_err());
        input.session_epoch = "x".repeat(257);
        assert!(input.validate().is_err());
    }
}
