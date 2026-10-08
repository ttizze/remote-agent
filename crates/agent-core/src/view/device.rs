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
    pub session_epoch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DeviceFrameView {
    pub thread_id: String,
    pub session_epoch: String,
    pub host_id: String,
    pub device_id: String,
    pub platform: String,
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DeviceVideoFrameView {
    pub thread_id: String,
    pub session_epoch: String,
    pub host_id: String,
    pub device_id: String,
    pub platform: String,
    pub payload: Vec<u8>,
    pub encoding: String,
    pub width: u32,
    pub height: u32,
    pub sequence: u64,
    pub timestamp_us: Option<u64>,
    pub keyframe: bool,
    pub screen_id: Option<u8>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DeviceAccessibilityView {
    pub host_id: String,
    pub device_id: String,
    pub session_epoch: String,
    pub elements: Vec<DeviceAccessibilityElementView>,
    pub errors: Vec<String>,
    pub read_at: String,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DeviceAccessibilityElementView {
    pub id: String,
    pub label: String,
    pub role: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DeviceForegroundView {
    pub host_id: String,
    pub device_id: String,
    pub session_epoch: String,
    pub app_id: Option<String>,
    pub received_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DeviceEventLogView {
    pub host_id: String,
    pub device_id: String,
    pub session_epoch: String,
    pub id: u64,
    pub timestamp: String,
    pub kind: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DeviceScreenView {
    pub thread_id: Option<String>,
    pub session_epoch: String,
    pub host_id: Option<String>,
    pub device_id: Option<String>,
    pub width: u32,
    pub height: u32,
    pub orientation: String,
    pub screen_id: Option<u8>,
    pub hinge_angle: Option<f32>,
    pub hinge_pose: Option<String>,
    pub table_mode: bool,
    pub table_mode_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DeviceRecordingView {
    pub thread_id: String,
    pub host_id: String,
    pub device_id: String,
    pub format: String,
    pub file_name: String,
    pub mime_type: String,
    pub started_at: String,
    pub frame_count: u64,
    pub byte_count: u64,
    pub error: Option<String>,
    pub bytes: Vec<u8>,
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
    /// Changes for every accepted frame/video event and stream prune.
    pub frame_revision: u64,
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
    pub video_frames: Vec<DeviceVideoFrameView>,
    /// Ordered access units for stateful native decoders.
    pub video_events: Vec<DeviceVideoFrameView>,
    pub accessibility: Vec<DeviceAccessibilityView>,
    pub foreground: Vec<DeviceForegroundView>,
    pub event_log: Vec<DeviceEventLogView>,
    pub screens: Vec<DeviceScreenView>,
    pub recordings: Vec<DeviceRecordingView>,
    pub last_recording: Option<DeviceRecordingView>,
    pub duo_controls: Vec<DeviceDuoControlView>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DeviceDuoControlView {
    pub thread_id: String,
    pub host_id: String,
    pub device_id: String,
    pub session_epoch: String,
    pub pending: bool,
    pub requested: Option<String>,
    pub error: Option<String>,
}

pub fn device_view(snapshot: &Snapshot) -> DeviceView {
    let state = &snapshot.device;
    let service = state.service();
    DeviceView {
        revision: service.revision,
        frame_revision: state.frame_revision,
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
                session_epoch: session.session_epoch.clone(),
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
                session_epoch: frame.session_epoch.clone(),
                host_id: frame.device.host_id.clone(),
                device_id: frame.device.id.clone(),
                platform: platform_name(frame.device.platform).into(),
                png: frame.png.clone(),
                width: frame.width,
                height: frame.height,
                sequence: frame.sequence,
            })
            .collect(),
        video_frames: state
            .video_frames
            .values()
            .map(video_frame_view)
            .collect(),
        video_events: state
            .video_events
            .values()
            .flat_map(|frames| frames.iter().map(video_frame_view))
            .collect(),
        accessibility: state
            .accessibility
            .values()
            .map(|tree| DeviceAccessibilityView {
                host_id: tree.host_id.clone(),
                device_id: tree.device_id.clone(),
                session_epoch: tree.session_epoch.clone(),
                elements: tree
                    .elements
                    .iter()
                    .map(|element| DeviceAccessibilityElementView {
                        id: element.id.clone(),
                        label: element.label.clone(),
                        role: element.role.clone(),
                        x: element.x,
                        y: element.y,
                        width: element.width,
                        height: element.height,
                    })
                    .collect(),
                errors: tree.errors.clone(),
                read_at: tree.read_at.clone(),
            })
            .collect(),
        foreground: state
            .foreground
            .values()
            .map(|update| DeviceForegroundView {
                host_id: update.host_id.clone(),
                device_id: update.device_id.clone(),
                session_epoch: update.session_epoch.clone(),
                app_id: update.app.as_ref().map(|app| app.id.clone()),
                received_at: update.received_at.clone(),
            })
            .collect(),
        event_log: state
            .event_log
            .values()
            .flat_map(|entries| entries.iter())
            .map(|entry| DeviceEventLogView {
                host_id: entry.host_id.clone(),
                device_id: entry.device_id.clone(),
                session_epoch: entry.session_epoch.clone(),
                id: entry.id,
                timestamp: entry.timestamp.clone(),
                kind: entry.kind.clone(),
                summary: entry.summary.clone(),
            })
            .collect(),
        screens: state
            .screens
            .values()
            .map(|screen| DeviceScreenView {
                thread_id: screen.thread_id.as_ref().map(ToString::to_string),
                session_epoch: screen.session_epoch.clone(),
                host_id: screen.host_id.clone(),
                device_id: screen.device_id.clone(),
                width: screen.width,
                height: screen.height,
                orientation: orientation_name(screen.orientation).into(),
                screen_id: screen.screen_id,
                hinge_angle: screen.hinge_angle,
                hinge_pose: screen.hinge_pose.clone(),
                table_mode: screen.table_mode,
                table_mode_available: screen.table_mode_available,
            })
            .collect(),
        recordings: state
            .recordings
            .values()
            .map(|status| DeviceRecordingView {
                thread_id: status.thread_id.to_string(),
                host_id: status.host_id.clone(),
                device_id: status.device_id.clone(),
                format: recording_format_name(status.format).into(),
                file_name: status.file_name.clone(),
                mime_type: status.mime_type.clone(),
                started_at: status.started_at.clone(),
                frame_count: status.frame_count,
                byte_count: status.byte_count,
                error: status.error.clone(),
                bytes: vec![],
            })
            .collect(),
        last_recording: state.last_recording.as_ref().map(|recording| DeviceRecordingView {
            thread_id: recording.status.thread_id.to_string(),
            host_id: recording.status.host_id.clone(),
            device_id: recording.status.device_id.clone(),
            format: recording_format_name(recording.status.format).into(),
            file_name: recording.status.file_name.clone(),
            mime_type: recording.status.mime_type.clone(),
            started_at: recording.status.started_at.clone(),
            frame_count: recording.status.frame_count,
            byte_count: recording.status.byte_count,
            error: recording.status.error.clone(),
            bytes: recording.bytes.clone(),
        }),
        duo_controls: state
            .duo_controls
            .iter()
            .map(|((thread_id, host_id, device_id, session_epoch), control)| DeviceDuoControlView {
                thread_id: thread_id.clone(),
                host_id: host_id.clone(),
                device_id: device_id.clone(),
                session_epoch: session_epoch.clone(),
                pending: control.pending,
                requested: control.requested.as_ref().map(device_duo_command_name),
                error: control.error.clone(),
            })
            .collect(),
        error: state.error.clone(),
    }
}

fn video_frame_view(frame: &agent_protocol::device::DeviceVideoFrame) -> DeviceVideoFrameView {
    DeviceVideoFrameView {
        thread_id: frame.thread_id.to_string(),
        session_epoch: frame.session_epoch.clone(),
        host_id: frame.device.host_id.clone(),
        device_id: frame.device.id.clone(),
        platform: platform_name(frame.device.platform).into(),
        payload: frame.payload.clone(),
        encoding: video_encoding_name(frame.encoding).into(),
        width: frame.width,
        height: frame.height,
        sequence: frame.sequence,
        timestamp_us: frame.timestamp_us,
        keyframe: frame.keyframe,
        screen_id: frame.screen_id,
    }
}

fn device_duo_command_name(command: &crate::state::DeviceDuoCommandIntent) -> String {
    match command {
        crate::state::DeviceDuoCommandIntent::Angle { value } => format!("angle:{value:.1}"),
        crate::state::DeviceDuoCommandIntent::Pose { value } => format!("pose:{value:?}"),
        crate::state::DeviceDuoCommandIntent::Table { value } => format!("table:{value}"),
        crate::state::DeviceDuoCommandIntent::Physical { value } => format!("physical:{value:?}"),
        crate::state::DeviceDuoCommandIntent::Orientation { value } => format!("orientation:{value:?}"),
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

fn video_encoding_name(encoding: agent_protocol::device::DeviceFrameEncoding) -> &'static str {
    match encoding {
        agent_protocol::device::DeviceFrameEncoding::AvccDescription => "avcc-description",
        agent_protocol::device::DeviceFrameEncoding::H264 => "h264",
        agent_protocol::device::DeviceFrameEncoding::Mjpeg => "mjpeg",
        agent_protocol::device::DeviceFrameEncoding::Jpeg => "jpeg",
        agent_protocol::device::DeviceFrameEncoding::Png => "png",
        agent_protocol::device::DeviceFrameEncoding::Semu => "semu",
    }
}

fn orientation_name(orientation: agent_protocol::device::DeviceOrientation) -> &'static str {
    match orientation {
        agent_protocol::device::DeviceOrientation::Portrait => "portrait",
        agent_protocol::device::DeviceOrientation::PortraitUpsideDown => "portrait_upside_down",
        agent_protocol::device::DeviceOrientation::LandscapeLeft => "landscape_left",
        agent_protocol::device::DeviceOrientation::LandscapeRight => "landscape_right",
    }
}

fn recording_format_name(format: agent_protocol::device::DeviceRecordingFormat) -> &'static str {
    match format {
        agent_protocol::device::DeviceRecordingFormat::Mp4 => "mp4",
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
