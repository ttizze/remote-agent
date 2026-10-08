//! Device panel views. The Host owns discovery and actions; this module only
//! turns the folded protocol state into binding-friendly records.
use crate::state::Snapshot;

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DeviceHostView {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub target: Option<String>,
    pub identity_file: Option<String>,
    pub port: Option<u16>,
    pub platforms: Vec<String>,
    pub unavailable_reasons: Vec<String>,
    pub hub_installed: bool,
    pub agent_device_installed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DeviceEntryView {
    pub host_id: String,
    pub id: String,
    pub platform: String,
    pub name: String,
    pub version: String,
    pub booted: bool,
    pub physical: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DeviceSessionView {
    pub thread_id: String,
    pub host_id: String,
    pub device_id: String,
    pub platform: String,
    pub opened_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DeviceFrameView {
    pub thread_id: String,
    pub host_id: String,
    pub device_id: String,
    pub platform: String,
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DeviceDetailView {
    pub host_id: String,
    pub device_id: String,
    pub read_at: String,
    pub appearance: Option<String>,
    pub text_size: Option<String>,
    pub reduce_motion: Option<bool>,
    pub increase_contrast: Option<bool>,
    pub reduce_transparency: Option<bool>,
    pub show_borders: Option<bool>,
    pub voice_over: Option<bool>,
    pub liquid_glass: Option<String>,
    pub color_filter: Option<String>,
    pub network_enabled: Option<bool>,
    pub location_latitude: Option<f64>,
    pub location_longitude: Option<f64>,
    pub foreground_app: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DeviceView {
    pub revision: u64,
    pub status: String,
    pub status_detail: Option<String>,
    pub enabled: bool,
    pub agent_access_enabled: bool,
    pub onboarding_completed: bool,
    pub hosts: Vec<DeviceHostView>,
    pub devices: Vec<DeviceEntryView>,
    pub sessions: Vec<DeviceSessionView>,
    pub details: Vec<DeviceDetailView>,
    pub frames: Vec<DeviceFrameView>,
    pub error: Option<String>,
}

pub fn device_view(snapshot: &Snapshot) -> DeviceView {
    let state = &snapshot.device;
    let service = state.service();
    DeviceView {
        revision: service.revision,
        status: status_name(service.host_status).into(),
        status_detail: service.host_status_detail,
        enabled: service.host_status != agent_protocol::device::DeviceHostStatus::Disabled,
        agent_access_enabled: service.agent_access_enabled,
        onboarding_completed: service.onboarding_completed,
        hosts: service
            .hosts
            .into_iter()
            .map(|host| DeviceHostView {
                id: host.id,
                kind: kind_name(host.kind).into(),
                label: host.label,
                target: host.target,
                identity_file: host.identity_file,
                port: host.port,
                platforms: host
                    .platforms
                    .iter()
                    .filter(|platform| platform.available)
                    .map(|platform| platform_name(platform.platform).into())
                    .collect(),
                unavailable_reasons: host
                    .platforms
                    .into_iter()
                    .filter_map(|platform| platform.reason)
                    .collect(),
                hub_installed: host.hub_installed,
                agent_device_installed: host.agent_device_installed,
            })
            .collect(),
        devices: service
            .devices
            .into_iter()
            .map(|device| DeviceEntryView {
                host_id: device.host_id,
                id: device.id,
                platform: platform_name(device.platform).into(),
                name: device.name,
                version: device.version,
                booted: device.booted,
                physical: device.physical,
            })
            .collect(),
        sessions: state
            .sessions
            .iter()
            .map(|session| DeviceSessionView {
                thread_id: session.thread_id.to_string(),
                host_id: session.host_id.clone(),
                device_id: session.device_id.clone(),
                platform: platform_name(session.platform).into(),
                opened_at: session.opened_at.clone(),
            })
            .collect(),
        details: state
            .details
            .values()
            .map(|detail| DeviceDetailView {
                host_id: detail.host_id.clone(),
                device_id: detail.device_id.clone(),
                read_at: detail.read_at.clone(),
                appearance: detail.settings.appearance.map(|value| match value {
                    agent_protocol::device::DeviceAppearance::Light => "light".into(),
                    agent_protocol::device::DeviceAppearance::Dark => "dark".into(),
                }),
                text_size: detail.settings.text_size.map(|value| match value {
                    agent_protocol::device::DeviceTextSize::Small => "small".into(),
                    agent_protocol::device::DeviceTextSize::Default => "default".into(),
                    agent_protocol::device::DeviceTextSize::Large => "large".into(),
                    agent_protocol::device::DeviceTextSize::ExtraLarge => "extra-large".into(),
                }),
                reduce_motion: detail.settings.reduce_motion,
                increase_contrast: detail.settings.increase_contrast,
                reduce_transparency: detail.settings.reduce_transparency,
                show_borders: detail.settings.show_borders,
                voice_over: detail.settings.voice_over,
                liquid_glass: detail.settings.liquid_glass.clone(),
                color_filter: detail.settings.color_filter.map(|value| match value {
                    agent_protocol::device::DeviceColorFilter::None => "none".into(),
                    agent_protocol::device::DeviceColorFilter::Grayscale => "grayscale".into(),
                    agent_protocol::device::DeviceColorFilter::RedGreen => "red-green".into(),
                    agent_protocol::device::DeviceColorFilter::GreenRed => "green-red".into(),
                    agent_protocol::device::DeviceColorFilter::BlueYellow => "blue-yellow".into(),
                }),
                network_enabled: detail.settings.network_enabled,
                location_latitude: detail.settings.location.map(|location| location.0),
                location_longitude: detail.settings.location.map(|location| location.1),
                foreground_app: detail.foreground_app.as_ref().map(|app| app.id.clone()),
            })
            .collect(),
        frames: state
            .frames
            .values()
            .map(|frame| DeviceFrameView {
                thread_id: frame.thread_id.to_string(),
                host_id: frame.device.host_id.clone(),
                device_id: frame.device.id.clone(),
                platform: platform_name(frame.device.platform).into(),
                png: frame.png.clone(),
                width: frame.width,
                height: frame.height,
                sequence: frame.sequence,
            })
            .collect(),
        error: state.error.clone(),
    }
}

fn platform_name(platform: agent_protocol::device::DevicePlatform) -> &'static str {
    match platform {
        agent_protocol::device::DevicePlatform::Ios => "ios",
        agent_protocol::device::DevicePlatform::Android => "android",
    }
}
fn kind_name(kind: agent_protocol::device::DeviceHostKind) -> &'static str {
    match kind {
        agent_protocol::device::DeviceHostKind::Local => "local",
        agent_protocol::device::DeviceHostKind::Ssh => "ssh",
    }
}
fn status_name(status: agent_protocol::device::DeviceHostStatus) -> &'static str {
    match status {
        agent_protocol::device::DeviceHostStatus::Disabled => "disabled",
        agent_protocol::device::DeviceHostStatus::Idle => "idle",
        agent_protocol::device::DeviceHostStatus::Installing => "installing",
        agent_protocol::device::DeviceHostStatus::Starting => "starting",
        agent_protocol::device::DeviceHostStatus::Ready => "ready",
        agent_protocol::device::DeviceHostStatus::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_snapshot_has_disabled_device_support() {
        let view = device_view(&Snapshot::default());
        assert_eq!(view.status, "disabled");
        assert!(!view.enabled);
        assert!(view.hosts.is_empty());
    }
}
